//! `ls` 子命令(M1-WP07-T01 卡内 ②;T03 渲染面):rc 白名单 `operations/list`
//! 单目录列举,**fs+remote 两键齐传**(M1-WP05-T09 F2 实锚形状:仅 fs → 400
//! `Didn't find key "remote" in input`;`remote:""` 合法 = 列举 fs 根)。
//! 数据获取路径零改动(载荷零字段裁剪、零 schema 发明同口径);应答渲染为
//! 人类可读表格(T03:列 = 名称/类型/大小/修改时间,目录恒前排+名称排序)。

use partiverse_core::error::PartisyError;
use partiverse_core::jobs::RcDispatch;
use serde_json::Value;
use serde_json::json;

/// 目标解析规则(本卡起为 CLI 各子命令共用单源):冒号前 = remote 名(fs =
/// `名:`),冒号后 = 其内路径(可空 = 根);无冒号 = 裸本地路径(fs = 路径本身,
/// remote = "",T09 实锚「裸路径 fs + remote:"" → 200」)。
#[must_use]
pub(crate) fn split_target(target: &str) -> (String, String) {
    match target.split_once(':') {
        Some((name, path)) => (format!("{name}:"), path.to_owned()),
        None => (target.to_owned(), String::new()),
    }
}

/// `ls <remote:path>`:目标解析规则见 [`split_target`]。数据获取路径零改动
/// (operations/list fs+remote 两键齐传);应答经 [`render_listing`] 渲染为表格。
///
/// # Errors
/// rc 失败按引擎 severity 保真上浮(零吞错)。
pub fn ls(dispatch: &impl RcDispatch, target: &str) -> Result<String, PartisyError> {
    let (fs, remote) = split_target(target);
    let reply = dispatch.call("operations/list", &json!({ "fs": fs, "remote": remote }))?;
    Ok(render_listing(&reply).join("\n"))
}

/// 单条列表条目的展示投影(渲染器内部面,纯数据)。
struct ListingRow {
    is_dir: bool,
    name: String,
    kind: &'static str,
    size: String,
    mod_time: String,
}

/// 应答条目 → 展示投影(缺字段容错:Name 缺 → `-`;Size 缺/非非负整数 → `-`;
/// ModTime 缺 → `-`;IsDir 缺/非 true → 按文件,与排序口径一致)。
fn listing_row(raw: &Value) -> ListingRow {
    let is_dir = raw.get("IsDir").and_then(Value::as_bool) == Some(true);
    ListingRow {
        is_dir,
        name: raw
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or("-")
            .to_owned(),
        kind: if is_dir { "dir" } else { "file" },
        size: raw
            .get("Size")
            .and_then(Value::as_u64)
            .map_or_else(|| "-".to_owned(), format_size),
        mod_time: raw
            .get("ModTime")
            .and_then(Value::as_str)
            .map_or_else(|| "-".to_owned(), format_mod_time),
    }
}

/// operations/list 应答 → 表格行(纯函数,含表头;T03 卡内 ③)。
///
/// 口径:目录恒前排 + 名称排序(08 规划 §4.2);缺 `list` 键 = 空目录同形
/// (仅表头);列宽按内容自适应,大小列右对齐。零字段发明:条目仅读引擎
/// 原生 `Name/IsDir/Size/ModTime` 四键。
#[must_use]
pub(crate) fn render_listing(reply: &Value) -> Vec<String> {
    const HEADERS: [&str; 4] = ["NAME", "TYPE", "SIZE", "MODIFIED"];
    let mut rows: Vec<ListingRow> = reply
        .get("list")
        .and_then(Value::as_array)
        .map(|list| list.iter().map(listing_row).collect())
        .unwrap_or_default();
    // 目录恒前排(布尔序 false < true,倒置比较)+ 名称排序(字节序,确定性)。
    rows.sort_unstable_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    let name_w = rows
        .iter()
        .map(|row| row.name.chars().count())
        .chain(std::iter::once(HEADERS[0].len()))
        .max()
        .unwrap_or(HEADERS[0].len());
    let size_w = rows
        .iter()
        .map(|row| row.size.len())
        .chain(std::iter::once(HEADERS[2].len()))
        .max()
        .unwrap_or(HEADERS[2].len());
    let mut lines = vec![format!(
        "{:<name_w$}  {:<4}  {:>size_w$}  {}",
        HEADERS[0], HEADERS[1], HEADERS[2], HEADERS[3]
    )];
    for row in &rows {
        lines.push(format!(
            "{:<name_w$}  {:<4}  {:>size_w$}  {}",
            row.name, row.kind, row.size, row.mod_time
        ));
    }
    lines
}

/// 字节人类可读化(1024 进制,自实现零依赖;T03 卡内自由度声明):
/// <1024 整数字节 `B`;KiB/MiB/GiB/TiB 一位小数(超大值封顶 TiB,阶梯完整
/// 防 `1 MiB = 1024 KiB` 之外的畸形长串)。
#[must_use]
fn format_size(size: u64) -> String {
    const KIB: f64 = 1024.0;
    if size < 1024 {
        return format!("{size} B");
    }
    let units = ["KiB", "MiB", "GiB", "TiB"];
    // 预除一次进入 KiB 段(unit=0);此后 value >= KIB 才继续升档。
    let mut value = size as f64 / KIB;
    let mut unit = 0;
    while value >= KIB && unit < units.len() - 1 {
        value /= KIB;
        unit += 1;
    }
    format!("{value:.1} {}", units[unit])
}

/// ModTime 简式化:`RFC3339` → `YYYY-MM-DD HH:MM:SS[±HH:MM|Z]`(分数秒丢弃)。
/// 偏移段 = rclone 原样保留(rclone 按引擎进程本地时区输出 ModTime,即本地
/// 时区语义;零依赖下不做跨时区换算);形状不符原样透传(容错面)。
#[must_use]
fn format_mod_time(raw: &str) -> String {
    let Some((date, rest)) = raw.split_once(['T', ' ']) else {
        return raw.to_owned();
    };
    let bytes = rest.as_bytes();
    // 时间段 `HH:MM:SS` 形状核验(第 3/6 字节为冒号),不符 = 原样透传。
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' {
        return raw.to_owned();
    }
    let tail = &rest[8..];
    let offset = if tail.ends_with('Z') {
        "Z"
    } else {
        // 分数秒只有数字与点,首个 `+`/`-` 即时区偏移起点。
        tail.find(['+', '-']).map_or("", |start| &tail[start..])
    };
    format!("{date} {}{offset}", &rest[..8])
}

/// 目标解析规则单测(纯函数面;rc 形状由真引擎集成测试锚定)。
#[cfg(test)]
mod tests {
    use super::*;
    use partiverse_core::error::PartisyError;
    use partiverse_core::jobs::RcDispatch;
    use serde_json::Value;

    /// 解析契约:具名 remote(冒号前=名,fs 加尾冒号;冒号后=路径,可空)+
    /// 裸本地路径(fs=路径本身,remote="")。
    #[test]
    fn target_splitting_matches_t09_shape() {
        /// Fake* 测试替身(仅测试命名惯例):记录最近一次 rc 入参。
        struct FakeRecorder(std::sync::Mutex<Option<Value>>);

        impl RcDispatch for FakeRecorder {
            fn call(&self, method: &str, params: &Value) -> Result<Value, PartisyError> {
                assert_eq!(method, "operations/list");
                *self.0.lock().expect("recorder lock") = Some(params.clone());
                Ok(json!({}))
            }
        }

        let recorded = |recorder: &FakeRecorder| {
            recorder
                .0
                .lock()
                .expect("recorder lock")
                .clone()
                .expect("params recorded")
        };
        let recorder = FakeRecorder(std::sync::Mutex::new(None));
        ls(&recorder, "pvlocal:sub/dir").expect("named remote with path");
        assert_eq!(
            recorded(&recorder),
            json!({ "fs": "pvlocal:", "remote": "sub/dir" })
        );
        ls(&recorder, "pvlocal:").expect("named remote root");
        assert_eq!(
            recorded(&recorder),
            json!({ "fs": "pvlocal:", "remote": "" })
        );
        ls(&recorder, "/tmp/some/dir").expect("bare local path");
        assert_eq!(
            recorded(&recorder),
            json!({ "fs": "/tmp/some/dir", "remote": "" })
        );
    }

    /// 渲染契约 T03③:空目录 = 仅表头(表头行非 `{` 起始,人类可读面)。
    #[test]
    fn render_empty_listing_yields_header_only() {
        let lines = render_listing(&json!({ "list": [] }));
        assert_eq!(lines, ["NAME  TYPE  SIZE  MODIFIED"]);
        // `list` 键缺失同形(容错面,零 panic)。
        assert_eq!(render_listing(&json!({})), ["NAME  TYPE  SIZE  MODIFIED"]);
    }

    /// 渲染契约 T03③:目录恒前排 + 组内名称排序;列对齐含表头,类型列
    /// `dir|file`,大小列右对齐(引擎原生 Name/IsDir/Size/ModTime 四键)。
    #[test]
    fn render_listing_sorts_dirs_first_then_names() {
        let lines = render_listing(&json!({
            "list": [
                { "Name": "zeta.txt", "IsDir": false, "Size": 1024,
                  "ModTime": "2026-10-11T08:30:05.5+08:00" },
                { "Name": "beta", "IsDir": true, "Size": 0,
                  "ModTime": "2026-10-10T23:59:59Z" },
                { "Name": "alpha.txt", "IsDir": false, "Size": 15,
                  "ModTime": "2026-10-11T08:00:00+08:00" },
                { "Name": "adam", "IsDir": true, "Size": 0,
                  "ModTime": "2026-10-11T00:00:00+08:00" }
            ]
        }));
        let names: Vec<&str> = lines[1..]
            .iter()
            .map(|line| line.split_whitespace().next().expect("row has name"))
            .collect();
        assert_eq!(names, ["adam", "beta", "alpha.txt", "zeta.txt"]);
        assert_eq!(
            lines,
            [
                "NAME       TYPE     SIZE  MODIFIED",
                "adam       dir       0 B  2026-10-11 00:00:00+08:00",
                "beta       dir       0 B  2026-10-10 23:59:59Z",
                "alpha.txt  file     15 B  2026-10-11 08:00:00+08:00",
                "zeta.txt   file  1.0 KiB  2026-10-11 08:30:05+08:00",
            ],
            "列对齐含表头(名称列左对齐/大小列右对齐)+ ModTime 简式化"
        );
    }

    /// 字节格式化边界(卡内 ③:0/1023/1024/1 MiB;1024 进制,一位小数阶梯)。
    #[test]
    fn format_size_boundaries() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.0 KiB");
        assert_eq!(format_size(1536), "1.5 KiB");
        assert_eq!(format_size(1024 * 1024), "1.0 MiB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.0 GiB");
    }

    /// 缺字段容错(卡内 ③):非目录条目缺 Size/ModTime → `-`,IsDir 缺 →
    /// 按文件;ModTime 形状不符原样透传。
    #[test]
    fn render_listing_tolerates_missing_fields() {
        let lines = render_listing(&json!({
            "list": [{ "Name": "bare.txt" },
                     { "Name": "odd.txt", "IsDir": false, "Size": 7,
                       "ModTime": "not-a-timestamp" }]
        }));
        assert_eq!(
            lines,
            [
                "NAME      TYPE  SIZE  MODIFIED",
                "bare.txt  file     -  -",
                "odd.txt   file   7 B  not-a-timestamp",
            ],
            "Size/ModTime 缺失 → `-`;ModTime 形状不符原样透传"
        );
    }
}
