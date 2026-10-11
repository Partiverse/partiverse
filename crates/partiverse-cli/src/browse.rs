//! `ls` 子命令(M1-WP07-T01 卡内 ②):rc 白名单 `operations/list` 单目录列举,
//! **fs+remote 两键齐传**(M1-WP05-T09 F2 实锚形状:仅 fs → 400 `Didn't find
//! key "remote" in input`;`remote:""` 合法 = 列举 fs 根)。载荷 JSON 原样透传
//! (壳零字段裁剪、零 schema 发明同口径)。

use partiverse_core::error::PartisyError;
use partiverse_core::jobs::RcDispatch;
use serde_json::json;

/// `ls <remote:path>`:目标解析规则——冒号前 = remote 名(fs = `名:`),冒号后
/// = 其内路径(可空 = 根);无冒号 = 裸本地路径(fs = 路径本身,remote = "",
/// T09 实锚「裸路径 fs(local 后端)+ remote:"" → 200」)。
///
/// # Errors
/// rc 失败按引擎 severity 保真上浮(零吞错)。
pub fn ls(dispatch: &impl RcDispatch, target: &str) -> Result<String, PartisyError> {
    let (fs, remote) = match target.split_once(':') {
        Some((name, path)) => (format!("{name}:"), path),
        None => (target.to_owned(), ""),
    };
    let reply = dispatch.call("operations/list", &json!({ "fs": fs, "remote": remote }))?;
    Ok(reply.to_string())
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
}
