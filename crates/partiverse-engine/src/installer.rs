//! 引擎安装器(M1-WP02-T01):锁定版本下载 / sha256 校验 / 多源回退 / 手动导入。
//!
//! 布局 `<install_root>/<version>/<binary>`,旁挂 `<binary>.sha256` 留档二进制实测
//! 摘要(防漂移;最终锚定=manifest 的 zip sha256),支撑 `ensure()` 幂等快路径。
//! 零吞错:单源失败记录 `SourceAttempt` 换下一源;全部失败聚合 `DownloadFailed`
//! (Retryable);安装根/落盘 IO 属环境问题,按 `classify_io` 分级立即上浮。
//! 含安装根解析(任务卡 ⑤)与 sha256 工具。

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::{Digest, Sha256};
use zip::ZipArchive;

use crate::error::{EngineError, EngineErrorKind, SourceAttempt, SourceFailureKind};
use crate::hash::{decode, encode};
use crate::manifest::{EngineManifest, PlatformAsset};
use crate::source::{HttpSource, PackageSource};

/// 安装根环境变量覆盖(任务卡 ⑤ 第一优先级)。
pub const ENGINE_ROOT_ENV: &str = "PARTIVERSE_ENGINE_ROOT";

/// 下载临时目录名(install_root 下;残留可被后续尝试截断覆盖,无害)。
const DOWNLOAD_TMP_DIR: &str = ".partiverse-download";

/// 流式读写缓冲区大小。
const COPY_CHUNK: usize = 64 * 1024;

/// 引擎安装器:持有解析后的锁定版本 manifest。
#[derive(Debug)]
pub struct EngineInstaller {
    manifest: EngineManifest,
}

impl EngineInstaller {
    /// 以给定 manifest 构造安装器。
    #[must_use]
    pub fn new(manifest: EngineManifest) -> Self {
        EngineInstaller { manifest }
    }

    /// 以随仓内嵌 manifest 构造安装器(生产入口)。
    pub fn from_embedded_manifest() -> Result<Self, EngineError> {
        Ok(EngineInstaller::new(EngineManifest::embedded()?))
    }

    /// 安装根解析(任务卡 ⑤):env `PARTIVERSE_ENGINE_ROOT` > 平台数据目录。
    pub fn resolve_install_root() -> Result<PathBuf, EngineError> {
        if let Some(root) = std::env::var_os(ENGINE_ROOT_ENV) {
            // 空串视同未设置(避免空路径在下游产生费解错误)。
            if !root.is_empty() {
                return Ok(PathBuf::from(root));
            }
        }
        platform_data_dir()
            .map(|dir| dir.join("partiverse").join("engine"))
            .ok_or_else(|| {
                EngineError::new(
                    EngineErrorKind::InstallRootUnresolved(
                        "neither env override nor platform data/home directory available".into(),
                    ),
                    partiverse_core::error::Severity::Fatal,
                )
            })
    }

    /// 幂等确保引擎就绪(任务卡 ②):已存在且 sha256 复验通过 → 幂等返回;缺失/
    /// 损坏 → 按源序下载 zip → manifest sha256 校验 → 解包落盘(executable)。
    pub fn ensure(&self, install_root: &Path) -> Result<PathBuf, EngineError> {
        let asset = self.manifest.current_platform()?;
        if let Some(path) = self.verified_binary_path(install_root, asset)? {
            return Ok(path);
        }
        // 生产源 = manifest 有序源逐个配对 HTTP 客户端(与测试共用同一条回退路径)。
        let expected_sha = expected_zip_sha(asset)?;
        let http_sources: Vec<HttpSource> = self
            .manifest
            .sources
            .iter()
            .map(|_| HttpSource::new())
            .collect();
        let paired: Vec<(&crate::manifest::DownloadSource, &dyn PackageSource)> = self
            .manifest
            .sources
            .iter()
            .zip(&http_sources)
            .map(|(manifest_source, source)| (manifest_source, source as &dyn PackageSource))
            .collect();
        self.install_from_sources(install_root, asset, expected_sha, &paired)
    }

    /// 手动导入本地发行包(D31 离线安装,任务卡 ④):先经 manifest sha256 复验
    /// (不符 → Fatal),通过后解包落盘;校验与下载路径一致,无跳过校验的代码路径。
    pub fn import_engine(
        &self,
        install_root: &Path,
        zip_path: &Path,
    ) -> Result<PathBuf, EngineError> {
        let asset = self.manifest.current_platform()?;
        let expected_sha = expected_zip_sha(asset)?;
        let actual_sha = sha256_file(zip_path)?;
        if actual_sha != expected_sha {
            return Err(EngineError::new(
                EngineErrorKind::ImportHashMismatch {
                    expected: encode(expected_sha),
                    actual: encode(actual_sha),
                },
                partiverse_core::error::Severity::Fatal,
            ));
        }
        self.install_from_zip(install_root, asset, zip_path)
    }

    /// 快路径复验:二进制存在、留档存在且与实测摘要一致才返回 Some。
    fn verified_binary_path(
        &self,
        install_root: &Path,
        asset: &PlatformAsset,
    ) -> Result<Option<PathBuf>, EngineError> {
        let binary = self.binary_path(install_root, asset);
        if !binary.is_file() {
            return Ok(None);
        }
        let recorded = match fs::read_to_string(sidecar_path(&binary)) {
            // 无留档/留档损坏:无完整性锚点,回重装路径(重装即自愈,非致命)。
            Ok(text) => decode(text.trim()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => return Err(EngineError::from_io(err)),
        };
        let Some(recorded) = recorded else {
            return Ok(None);
        };
        Ok((sha256_file(&binary)? == recorded).then_some(binary))
    }

    /// 有序多源回退安装(任务卡 ③):单源失败自动切换下一源,全部失败聚合上浮。
    fn install_from_sources(
        &self,
        install_root: &Path,
        asset: &PlatformAsset,
        expected_sha: [u8; 32],
        sources: &[(&crate::manifest::DownloadSource, &dyn PackageSource)],
    ) -> Result<PathBuf, EngineError> {
        let tmp_dir = install_root.join(DOWNLOAD_TMP_DIR);
        fs::create_dir_all(&tmp_dir).map_err(EngineError::from_io)?;
        let tmp_zip = tmp_dir.join(&asset.zip_name);
        let mut attempts: Vec<SourceAttempt> = Vec::new();
        for (manifest_source, source) in sources {
            // 清理上一轮残留(NotFound 属正常;其余 IO 错误按环境问题上浮)。
            match fs::remove_file(&tmp_zip) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return Err(EngineError::from_io(err));
                }
                _ => {}
            }
            // 完整 URL = 去尾斜杠基址 + "/" + zip_name(URL 全部来自 manifest,零硬编码端点)。
            let url = format!(
                "{}/{}",
                manifest_source.base_url.trim_end_matches('/'),
                asset.zip_name
            );
            let mut file = File::create(&tmp_zip).map_err(EngineError::from_io)?;
            match source.fetch_into(&url, &mut file) {
                Ok(()) => {
                    drop(file);
                    // 传输不换校验(D31):下载产物必须对 manifest sha256 复验。
                    match sha256_file(&tmp_zip) {
                        Err(err) => return Err(err),
                        Ok(actual) if actual == expected_sha => {
                            match self.install_from_zip(install_root, asset, &tmp_zip) {
                                Ok(path) => return Ok(path),
                                Err(err) => match err.kind() {
                                    // 内容过哈希但解包失败:该源内容损坏 → 换源。
                                    EngineErrorKind::ExtractFailed(detail) => {
                                        attempts.push(SourceAttempt {
                                            source: manifest_source.name.clone(),
                                            url,
                                            cause: SourceFailureKind::ExtractFailed(detail.clone()),
                                        })
                                    }
                                    // 其余(落盘 IO 环境问题)换源无解,立即上浮。
                                    _ => return Err(err),
                                },
                            }
                        }
                        Ok(actual) => attempts.push(SourceAttempt {
                            source: manifest_source.name.clone(),
                            url,
                            cause: SourceFailureKind::HashMismatch {
                                expected: encode(expected_sha),
                                actual: encode(actual),
                            },
                        }),
                    }
                }
                Err(cause) => attempts.push(SourceAttempt {
                    source: manifest_source.name.clone(),
                    url,
                    cause,
                }),
            }
        }
        Err(EngineError::new(
            EngineErrorKind::DownloadFailed(attempts),
            partiverse_core::error::Severity::Retryable,
        ))
    }

    /// 从已通过 sha256 校验的 zip 解包引擎二进制并落盘(下载回退与 import 共用)。
    fn install_from_zip(
        &self,
        install_root: &Path,
        asset: &PlatformAsset,
        zip_path: &Path,
    ) -> Result<PathBuf, EngineError> {
        let file = File::open(zip_path).map_err(EngineError::from_io)?;
        let mut archive = ZipArchive::new(file).map_err(|err| {
            EngineError::new(
                EngineErrorKind::ExtractFailed(err.to_string()),
                partiverse_core::error::Severity::Fatal,
            )
            .with_source(Box::new(err))
        })?;
        let binary_index = locate_binary_entry(&mut archive, asset)?;
        let binary = self.binary_path(install_root, asset);
        let parent = binary.parent().ok_or_else(|| {
            EngineError::new(
                EngineErrorKind::ExtractFailed(format!(
                    "binary path has no parent: {}",
                    binary.display()
                )),
                partiverse_core::error::Severity::Fatal,
            )
        })?;
        fs::create_dir_all(parent).map_err(EngineError::from_io)?;
        // 解包到 <binary>.part 临时文件,完成后原子改名:中途失败不留半装状态;
        // 失败残留 .part 由下次尝试截断覆盖(不在此强删,避免清理错误掩盖主错误)。
        let part_path = part_file_path(&binary);
        let digest = extract_binary(&mut archive, binary_index, &part_path)?;
        fs::rename(&part_path, &binary).map_err(EngineError::from_io)?;
        fs::write(sidecar_path(&binary), format!("{}\n", encode(digest)))
            .map_err(EngineError::from_io)?;
        Ok(binary)
    }

    /// 安装布局:`<install_root>/<version>/<binary_name>`。
    fn binary_path(&self, install_root: &Path, asset: &PlatformAsset) -> PathBuf {
        install_root
            .join(&self.manifest.version)
            .join(&asset.binary_name)
    }
}

/// manifest 锚定的平台 zip sha256(解析期已验形;此处取值,非法即 Fatal)。
fn expected_zip_sha(asset: &PlatformAsset) -> Result<[u8; 32], EngineError> {
    decode(&asset.sha256).ok_or_else(|| {
        EngineError::new(
            EngineErrorKind::ManifestInvalid(format!("bad asset sha256: {}", asset.sha256)),
            partiverse_core::error::Severity::Fatal,
        )
    })
}

/// 定位引擎二进制条目:不假设 zip 顶层目录名,按路径末段 == binary_name 匹配
/// (官方布局 `rclone-v<版本>-<平台>/<binary>`,同目录另有 README/手册页)。
fn locate_binary_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    asset: &PlatformAsset,
) -> Result<usize, EngineError> {
    let fatal = |detail: String| {
        EngineError::new(
            EngineErrorKind::ExtractFailed(detail),
            partiverse_core::error::Severity::Fatal,
        )
    };
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|err| fatal(err.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().map_err(|err| fatal(err.to_string()))?;
        if Path::new(name.as_ref()).file_name() == Some(std::ffi::OsStr::new(&asset.binary_name)) {
            return Ok(index);
        }
    }
    Err(fatal(format!(
        "binary entry `{}` not found in archive",
        asset.binary_name
    )))
}

/// 流式解包二进制条目到 `part_path`(unix 置 0o755 可执行),边写边算摘要返回。
fn extract_binary<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    index: usize,
    part_path: &Path,
) -> Result<[u8; 32], EngineError> {
    let mut entry = archive.by_index(index).map_err(|err| {
        EngineError::new(
            EngineErrorKind::ExtractFailed(err.to_string()),
            partiverse_core::error::Severity::Fatal,
        )
    })?;
    let mut out = File::create(part_path).map_err(EngineError::from_io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let read = entry.read(&mut buf).map_err(|err| {
            EngineError::new(
                EngineErrorKind::ExtractFailed(format!("decompress failed: {err}")),
                partiverse_core::error::Severity::Fatal,
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
        out.write_all(&buf[..read]).map_err(EngineError::from_io)?;
    }
    out.flush().map_err(EngineError::from_io)?;
    // unix:引擎二进制必须可执行(0o755 = rwxr-xr-x);windows 无权限位概念。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.set_permissions(fs::Permissions::from_mode(0o755))
            .map_err(EngineError::from_io)?;
    }
    Ok(hasher.finalize().into())
}

/// `<binary>.sha256` 留档路径(逐字拼接,避免 with_extension 吞掉 `.exe` 后缀)。
fn sidecar_path(binary: &Path) -> PathBuf {
    let mut os = binary.as_os_str().to_os_string();
    os.push(".sha256");
    PathBuf::from(os)
}

/// `<binary>.part` 解包临时路径。
fn part_file_path(binary: &Path) -> PathBuf {
    let mut os = binary.as_os_str().to_os_string();
    os.push(".part");
    PathBuf::from(os)
}

/// 文件 sha256(分块流式,内存占用与文件大小无关)。
fn sha256_file(path: &Path) -> Result<[u8; 32], EngineError> {
    let mut file = File::open(path).map_err(EngineError::from_io)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        let read = file.read(&mut buf).map_err(EngineError::from_io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher.finalize().into())
}

/// 平台数据目录(任务卡 ⑤「Win/mac 各自已知目录」;ADR-0004:不引 dirs——其
/// windows-sys 版本链与 ring 必然错开,触发 deny multiple-versions="deny")。
/// linux=XDG_DATA_HOME(仅绝对路径,XDG 规范)→ $HOME/.local/share;
/// macOS=~/Library/Application Support;windows=%LOCALAPPDATA%(引擎二进制为
/// 设备本地资产,刻意不用 Roaming,避免大文件随漫游配置复制)。
fn platform_data_dir() -> Option<PathBuf> {
    fn env_dir(key: &str) -> Option<PathBuf> {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        env_dir("LOCALAPPDATA")
    }
    #[cfg(target_os = "macos")]
    {
        env_dir("HOME").map(|home| home.join("Library").join("Application Support"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        match env_dir("XDG_DATA_HOME") {
            Some(dir) if Path::new(&dir).is_absolute() => Some(dir),
            _ => env_dir("HOME").map(|home| home.join(".local").join("share")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::EngineErrorKind as Kind;
    use crate::manifest::DownloadSource;
    use zip::write::SimpleFileOptions;

    const TEST_VERSION: &str = "0.0.0-test";
    const FIXTURE_ZIP_NAME: &str = "fixture.zip";
    /// 测试专用自增序号(temp 目录唯一化,不引第三方 tempdir 依赖)。
    static TEMP_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// 测试临时目录(pid+序号唯一;测试收尾 `let _ = remove_dir_all` 自清理)。
    fn temp_dir(tag: &str) -> PathBuf {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "partiverse-engine-{tag}-{}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// 宿主平台二进制名(与 manifest 命名一致:windows=rclone.exe,其余=rclone)。
    fn host_binary_name() -> String {
        if EngineManifest::host_platform_key().starts_with("windows") {
            "rclone.exe".into()
        } else {
            "rclone".into()
        }
    }

    /// 构造 fixture zip(测试自造,非真实 rclone;deflate 与官方包同压缩),返回 (路径, sha hex)。
    fn write_fixture_zip(
        dir: &Path,
        tag: &str,
        entries: &[(String, Vec<u8>)],
    ) -> (PathBuf, String) {
        let zip_path = dir.join(format!("fixture-src-{tag}.zip"));
        let mut writer = zip::ZipWriter::new(File::create(&zip_path).expect("create fixture zip"));
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in entries {
            writer
                .start_file(name.as_str(), options)
                .expect("start zip entry");
            writer.write_all(content).expect("write zip entry");
        }
        writer.finish().expect("finish fixture zip");
        (
            zip_path.clone(),
            encode(sha256_file(&zip_path).expect("hash zip")),
        )
    }

    /// 官方布局形态的 fixture 条目(顶层目录 + 二进制 + 干扰文件)。
    fn official_entries(payload: &[u8]) -> Vec<(String, Vec<u8>)> {
        vec![
            (
                format!("engine-{TEST_VERSION}/{}", host_binary_name()),
                payload.to_vec(),
            ),
            (
                format!("engine-{TEST_VERSION}/README.txt"),
                b"fixture readme".to_vec(),
            ),
        ]
    }

    /// 组装测试 manifest JSON(zip 摘要 + 有序源表;a=主源,b=备源)。
    fn test_manifest_json(zip_sha_hex: &str, sources: &[&str]) -> String {
        let base = |name: &str| {
            if name == "a" {
                "https://fake.test/a/v"
            } else {
                "https://fake.test/b/v"
            }
        };
        let sources_json: String = sources
            .iter()
            .map(|name| format!(r#"{{"name":"{name}","base_url":"{}"}}"#, base(name)))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"version":"{TEST_VERSION}","platforms":{{"{}":{{"zip_name":"{FIXTURE_ZIP_NAME}","sha256":"{zip_sha_hex}","binary_name":"{}"}}}},"sources":[{sources_json}]}}"#,
            EngineManifest::host_platform_key(),
            host_binary_name(),
        )
    }

    /// fixture zip 的完整下载 URL(manifest 基址 + zip_name)。
    fn fixture_url(name: &str) -> String {
        let base = if name == "a" {
            "https://fake.test/a/v"
        } else {
            "https://fake.test/b/v"
        };
        format!("{base}/{FIXTURE_ZIP_NAME}")
    }

    /// Fake* 测试替身(仓库惯例命名):URL → 本地文件模拟传输,零真实网络;
    /// 无路由的 URL 自然产生 Transport 失败(可模拟单源断连)。
    #[derive(Debug)]
    struct FakeFileSource(std::collections::HashMap<String, PathBuf>);

    impl FakeFileSource {
        fn serving(url: &str, artifact: &Path) -> Self {
            let mut routes = std::collections::HashMap::new();
            routes.insert(url.to_string(), artifact.to_path_buf());
            FakeFileSource(routes)
        }
    }

    impl PackageSource for FakeFileSource {
        fn fetch_into(&self, url: &str, writer: &mut dyn Write) -> Result<(), SourceFailureKind> {
            let bytes = match self.0.get(url).map(fs::read) {
                Some(Ok(bytes)) => bytes,
                _ => {
                    return Err(SourceFailureKind::Transport(format!(
                        "no fake route: {url}"
                    )));
                }
            };
            writer
                .write_all(&bytes)
                .map_err(|err| SourceFailureKind::Transport(err.to_string()))
        }
    }

    /// 搭建夹具:(安装器, 安装根, fixture zip, fixture 目录)。测试收尾清 fixture 目录。
    fn build(
        tag: &str,
        payload: &[u8],
        sources: &[&str],
    ) -> (EngineInstaller, PathBuf, PathBuf, PathBuf) {
        let dir = temp_dir(tag);
        let (zip_path, zip_sha) = write_fixture_zip(&dir, tag, &official_entries(payload));
        let installer = EngineInstaller::new(
            EngineManifest::from_json(&test_manifest_json(&zip_sha, sources))
                .expect("manifest parses"),
        );
        (installer, dir.join("install"), zip_path, dir)
    }

    /// 把替身源按 manifest 源序配对(与 ensure() 的生产配对方式同构)。
    fn paired<'a>(
        installer: &'a EngineInstaller,
        fakes: &'a [&FakeFileSource],
    ) -> Vec<(&'a DownloadSource, &'a dyn PackageSource)> {
        installer
            .manifest
            .sources
            .iter()
            .zip(fakes)
            .map(|(manifest_source, fake)| (manifest_source, *fake as &dyn PackageSource))
            .collect()
    }

    /// 安装结果断言:二进制内容一致、unix 可执行、留档与实测一致;返回二进制路径。
    fn assert_installed(root: &Path, payload: &[u8]) -> PathBuf {
        let binary = root.join(TEST_VERSION).join(host_binary_name());
        assert_eq!(fs::read(&binary).expect("binary readable"), payload);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&binary)
                .expect("metadata")
                .permissions()
                .mode();
            assert_ne!(mode & 0o111, 0, "installed binary must be executable");
        }
        let recorded = fs::read_to_string(sidecar_path(&binary)).expect("sidecar written");
        assert_eq!(
            recorded.trim(),
            encode(sha256_file(&binary).expect("hash installed"))
        );
        binary
    }

    #[test]
    fn good_sha_falls_back_to_second_source_and_ensure_is_idempotent() {
        // 任务卡 ②/③(好 sha 例):主源断连 → 备源供包 → 可执行落盘 + 留档;
        // 再验 ensure 快路径幂等(不重装)。
        // (ensure 慢路径会真实联网,离线单测只覆盖其快路径与 install_from_sources。)
        let payload = b"fake rclone binary v1";
        let (installer, root, zip, dir) = build("good-sha", payload, &["a", "b"]);
        let primary = FakeFileSource::serving("/nonexistent-primary-route", Path::new("/dev/null"));
        let fallback = FakeFileSource::serving(&fixture_url("b"), &zip);
        let asset = installer.manifest.current_platform().expect("host asset");
        let expected = decode(&asset.sha256).expect("fixture sha");
        let path = installer
            .install_from_sources(
                &root,
                asset,
                expected,
                &paired(&installer, &[&primary, &fallback]),
            )
            .expect("second source must win");
        assert_eq!(path, assert_installed(&root, payload));
        assert_eq!(installer.ensure(&root).expect("ensure fast path"), path);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_sha_aggregates_retryable_with_all_source_causes() {
        // 任务卡 ③/⑥(坏 sha 例):全部源失败 → Retryable 聚合,逐源原因零吞错,无半装产物。
        // manifest 锚定好 fixture 的 sha,但两个源都不供正确内容。
        let (installer, root, _zip, dir) = build("bad-sha", b"payload", &["a", "b"]);
        // 主源:可达但内容哈希不符;备源:无路由(断连)。
        let wrong_zip = dir.join("wrong.zip");
        fs::write(&wrong_zip, b"wrong archive bytes").expect("write wrong artifact");
        let fakes = [
            FakeFileSource::serving(&fixture_url("a"), &wrong_zip),
            FakeFileSource::serving("/nonexistent-fallback-route", Path::new("/dev/null")),
        ];
        let refs: Vec<&FakeFileSource> = fakes.iter().collect();
        let asset = installer.manifest.current_platform().expect("host asset");
        let expected = decode(&asset.sha256).expect("fixture sha");
        let err = installer
            .install_from_sources(&root, asset, expected, &paired(&installer, &refs))
            .expect_err("all sources failed");
        assert_eq!(err.severity(), partiverse_core::error::Severity::Retryable);
        match err.kind() {
            Kind::DownloadFailed(attempts) => {
                assert_eq!(attempts.len(), 2, "every source attempt must be recorded");
                assert!(matches!(
                    attempts[0].cause,
                    SourceFailureKind::HashMismatch { .. }
                ));
                assert!(matches!(attempts[1].cause, SourceFailureKind::Transport(_)));
            }
            other => panic!("expected DownloadFailed, got {other:?}"),
        }
        assert!(
            !root.join(TEST_VERSION).exists(),
            "no half-installed binary"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_rejects_hash_mismatch_as_fatal() {
        // 任务卡 ④/⑥(手动导入):本地 zip sha256 与 manifest 不符 → Fatal 上浮。
        let (installer, root, _zip, dir) = build("import-bad", b"payload-a", &["a"]);
        let tampered = root.parent().expect("parent exists").join("tampered.zip");
        fs::write(&tampered, b"not a zip at all").expect("write tampered file");
        let err = installer
            .import_engine(&root, &tampered)
            .expect_err("must fail");
        assert_eq!(err.severity(), partiverse_core::error::Severity::Fatal);
        assert!(matches!(err.kind(), Kind::ImportHashMismatch { .. }));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_install_root_prefers_env_then_platform_data_dir() {
        // 任务卡 ⑤:env 覆盖 > 平台数据目录(XDG/Win/mac 已知目录)。
        let override_root = temp_dir("root-env");
        // SAFETY:测试进程内单线程读写该变量;其余测试不读 PARTIVERSE_ENGINE_ROOT。
        unsafe {
            std::env::set_var(ENGINE_ROOT_ENV, &override_root);
        }
        let resolved = EngineInstaller::resolve_install_root().expect("env override wins");
        // SAFETY:同上,收尾恢复环境。
        unsafe {
            std::env::remove_var(ENGINE_ROOT_ENV);
        }
        assert_eq!(resolved, override_root);
        let resolved = EngineInstaller::resolve_install_root();
        if platform_data_dir().is_some() {
            let root = resolved.expect("data dir available");
            assert!(root.ends_with(std::path::Path::new("partiverse").join("engine")));
        } else {
            assert!(matches!(
                resolved.expect_err("must fail").kind(),
                Kind::InstallRootUnresolved(_)
            ));
        }
        let _ = fs::remove_dir_all(&override_root);
    }
}
