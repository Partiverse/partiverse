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
        // 真机实锚(T05 验证):path 前导 `/` 会导致 rclone operations/list
        // 静默返回空列表(stat 宽容但 list 不宽容)——统一剥除。
        Some((name, path)) => (format!("{name}:"), path.trim_start_matches('/').to_owned()),
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
        .map(|row| display_width(&row.name))
        .chain(std::iter::once(display_width(HEADERS[0])))
        .max()
        .unwrap_or(display_width(HEADERS[0]));
    let size_w = rows
        .iter()
        .map(|row| row.size.len())
        .chain(std::iter::once(HEADERS[2].len()))
        .max()
        .unwrap_or(HEADERS[2].len());
    let mut lines = vec![format!(
        "{}  {:<4}  {:>size_w$}  {}",
        pad_left_aligned(HEADERS[0], name_w),
        HEADERS[1],
        HEADERS[2],
        HEADERS[3]
    )];
    for row in &rows {
        lines.push(format!(
            "{}  {:<4}  {:>size_w$}  {}",
            pad_left_aligned(&row.name, name_w),
            row.kind,
            row.size,
            row.mod_time
        ));
    }
    lines
}

/// 单字符终端显示宽(M1-WP07-T05 D4;自实现零依赖,禁引 unicode-width 等
/// 第三方包):Unicode 15.0 `EastAsianWidth.txt` 的 W(Wide)/F(Fullwidth)
/// 区段 = 2,其余(含 A/N/H/Na)= 1。区段表由 15.0.0 数据文件程序化提取并
/// 合并相邻区段(121 段,提取口径见 run 报告);默认 emoji 呈现区段
/// (U+1F300+ 等)恰落在 W 集内,故 emoji 宽 = 2,无需独立表。
/// 刻意不做的简化口径(零依赖下与本函数单源注释声明一致):组合字符/
/// 变体选择符按 1 计(不做零宽)、ZWJ 序列不合字、A(Ambiguous)按 1
/// 与既有 `chars().count()` 基线同值。
const WIDE_RANGES: [(u32, u32); 121] = [
    (0x1100, 0x115F), // Hangul Jamo 初声
    (0x231A, 0x231B), // 手表/沙漏(emoji 宽 2)
    (0x2329, 0x232A), // 〈〉书名号
    (0x23E9, 0x23EC),
    (0x23F0, 0x23F0),
    (0x23F3, 0x23F3),
    (0x25FD, 0x25FE),
    (0x2614, 0x2615),
    (0x2648, 0x2653),
    (0x267F, 0x267F),
    (0x2693, 0x2693),
    (0x26A1, 0x26A1),
    (0x26AA, 0x26AB),
    (0x26BD, 0x26BE),
    (0x26C4, 0x26C5),
    (0x26CE, 0x26CE),
    (0x26D4, 0x26D4),
    (0x26EA, 0x26EA),
    (0x26F2, 0x26F3),
    (0x26F5, 0x26F5),
    (0x26FA, 0x26FA),
    (0x26FD, 0x26FD),
    (0x2705, 0x2705),
    (0x270A, 0x270B),
    (0x2728, 0x2728),
    (0x274C, 0x274C),
    (0x274E, 0x274E),
    (0x2753, 0x2755),
    (0x2757, 0x2757),
    (0x2795, 0x2797),
    (0x27B0, 0x27B0),
    (0x27BF, 0x27BF),
    (0x2B1B, 0x2B1C),
    (0x2B50, 0x2B50),
    (0x2B55, 0x2B55),
    (0x2E80, 0x2E99), // CJK 部首补充
    (0x2E9B, 0x2EF3),
    (0x2F00, 0x2FD5), // 康熙部首
    (0x2FF0, 0x2FFB), // 表意文字描述符
    (0x3000, 0x303E), // CJK 符号标点(含全角空格)
    (0x3041, 0x3096), // 平假名
    (0x3099, 0x30FF), // 浊点/片假名
    (0x3105, 0x312F), // 注音
    (0x3131, 0x318E), // 谚文兼容字母
    (0x3190, 0x31E3), // 象形会意/注音扩展
    (0x31F0, 0x321E),
    (0x3220, 0x3247),
    (0x3250, 0x4DBF), // 带圈表意/CJK 扩展 A(连续 W 合并区段)
    (0x4E00, 0xA48C), // CJK 统一表意文字 + 彝文音节(连续 W 合并区段)
    (0xA490, 0xA4C6), // 彝文部首
    (0xA960, 0xA97C), // 谚文扩展 A
    (0xAC00, 0xD7A3), // 谚文音节
    (0xF900, 0xFAFF), // CJK 兼容表意
    (0xFE10, 0xFE19), // 竖排形式
    (0xFE30, 0xFE52), // CJK 兼容形式
    (0xFE54, 0xFE66),
    (0xFE68, 0xFE6B),
    (0xFF01, 0xFF60),   // 全角形式(F)
    (0xFFE0, 0xFFE6),   // 全角符号(F)
    (0x16FE0, 0x16FE4), // 西夏文计数/部首
    (0x16FF0, 0x16FF1),
    (0x17000, 0x187F7), // 西夏文
    (0x18800, 0x18CD5), // 西夏文部件
    (0x18D00, 0x18D08),
    (0x1AFF0, 0x1AFF3), // 假名扩展
    (0x1AFF5, 0x1AFFB),
    (0x1AFFD, 0x1AFFE),
    (0x1B000, 0x1B122), // 假名补充
    (0x1B132, 0x1B132),
    (0x1B150, 0x1B152),
    (0x1B155, 0x1B155),
    (0x1B164, 0x1B167),
    (0x1B170, 0x1B2FB), // 女书
    (0x1F004, 0x1F004), // 麻将(单点 W)
    (0x1F0CF, 0x1F0CF), // 扑克(单点 W)
    (0x1F18E, 0x1F18E),
    (0x1F191, 0x1F19A),
    (0x1F200, 0x1F202), // 补充符号与象形文字(默认 emoji 呈现 = 宽 2)
    (0x1F210, 0x1F23B),
    (0x1F240, 0x1F248),
    (0x1F250, 0x1F251),
    (0x1F260, 0x1F265),
    (0x1F300, 0x1F320),
    (0x1F32D, 0x1F335),
    (0x1F337, 0x1F37C),
    (0x1F37E, 0x1F393),
    (0x1F3A0, 0x1F3CA),
    (0x1F3CF, 0x1F3D3),
    (0x1F3E0, 0x1F3F0),
    (0x1F3F4, 0x1F3F4),
    (0x1F3F8, 0x1F43E),
    (0x1F440, 0x1F440),
    (0x1F442, 0x1F4FC),
    (0x1F4FF, 0x1F53D),
    (0x1F54B, 0x1F54E),
    (0x1F550, 0x1F567),
    (0x1F57A, 0x1F57A),
    (0x1F595, 0x1F596),
    (0x1F5A4, 0x1F5A4),
    (0x1F5FB, 0x1F64F),
    (0x1F680, 0x1F6C5),
    (0x1F6CC, 0x1F6CC),
    (0x1F6D0, 0x1F6D2),
    (0x1F6D5, 0x1F6D7),
    (0x1F6DC, 0x1F6DF),
    (0x1F6EB, 0x1F6EC),
    (0x1F6F4, 0x1F6FC),
    (0x1F7E0, 0x1F7EB),
    (0x1F7F0, 0x1F7F0),
    (0x1F90C, 0x1F93A),
    (0x1F93C, 0x1F945),
    (0x1F947, 0x1F9FF),
    (0x1FA70, 0x1FA7C),
    (0x1FA80, 0x1FA88),
    (0x1FA90, 0x1FABD),
    (0x1FABF, 0x1FAC5),
    (0x1FACE, 0x1FADB),
    (0x1FAE0, 0x1FAE8),
    (0x1FAF0, 0x1FAF8),
    (0x20000, 0x2FFFD), // CJK 扩展 B–F(第二平面)
    (0x30000, 0x3FFFD), // CJK 扩展 G–H(第三平面)
];

/// [`WIDE_RANGES`] 之外任一区段的字符宽 = 1(ASCII/拉丁/组合符等)。
#[must_use]
fn char_display_width(c: char) -> usize {
    let cp = c as u32;
    if WIDE_RANGES
        .iter()
        .any(|&(low, high)| cp >= low && cp <= high)
    {
        2
    } else {
        1
    }
}

/// 字符串终端显示宽(逐字符求和;列宽对齐的唯一口径)。
#[must_use]
fn display_width(text: &str) -> usize {
    text.chars().map(char_display_width).sum()
}

/// 名称列左对齐补空格(按显示宽补齐;Rust `{:<n$}` fill 对齐按 char 计数,
/// CJK/emoji 名会溢出列宽,故手工补空格,超宽时零补齐不截断)。
#[must_use]
fn pad_left_aligned(text: &str, width: usize) -> String {
    let mut padded = String::from(text);
    for _ in display_width(text)..width {
        padded.push(' ');
    }
    padded
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
    // 非 ASCII = 违背 rclone lsjson 契约的畸形串,原样透传(审查 [medium]:
    // 字节索引切片在多字节输入上会 panic;ASCII 核验一次到位,零 panic 承诺)。
    if bytes.len() < 8 || bytes[2] != b':' || bytes[5] != b':' || !rest.is_ascii() {
        return raw.to_owned();
    }
    let tail = &rest[8..];
    let offset = if tail.ends_with('Z') {
        "Z"
    } else {
        // 分数秒只有数字与点,首个 `+`/`-` 即时区偏移起点(ASCII 已核验,切片安全)。
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

    #[test]
    fn format_mod_time_never_panics_on_multibyte_malformed_input() {
        // 审查 [medium]:字节索引切片在多字节畸形 ModTime 上 panic 的回归防护。
        // 形状匹配(第 3/6 字节冒号)但切片点落在多字节字符内部 → 原样透传。
        let multibyte = "2026-10-11T12:34:5éfé";
        assert_eq!(format_mod_time(multibyte), multibyte);
        let multibyte_offset = "2026-10-11T12:34:56é+08:00";
        assert_eq!(format_mod_time(multibyte_offset), multibyte_offset);
        // 合法 ASCII 输入不受防护影响(既有行为回归)。
        assert_eq!(
            format_mod_time("2026-10-11T12:34:56.789+08:00"),
            "2026-10-11 12:34:56+08:00"
        );
    }

    /// 显示宽契约(D4 卡内 ①):纯 CJK/假名/谚文/全角 = 2/字(Unicode 15.0
    /// EastAsianWidth W/F);emoji(默认呈现区段)= 2;ASCII = 1;混合 = 逐字求和。
    #[test]
    fn display_width_cjk_mixed_and_emoji() {
        assert_eq!(display_width("数据目录"), 8, "纯 CJK = 2/字");
        assert_eq!(display_width("カナ"), 4, "假名 = W 区段宽 2");
        assert_eq!(display_width("한글"), 4, "谚文音节 = W 区段宽 2");
        assert_eq!(display_width("ＦＷ"), 4, "全角形式 = F 区段宽 2");
        assert_eq!(display_width("中文abc"), 7, "混合 = 逐字求和");
        assert_eq!(display_width("🌍"), 2, "emoji 宽 2");
        assert_eq!(display_width("a🌍b"), 4, "emoji 夹 ASCII = 逐字求和");
        assert_eq!(display_width("abc.txt"), 7, "ASCII = 1/字(旧口径同值)");
    }

    /// 渲染契约(D4 卡内 ①):CJK/emoji 名称列宽按显示宽计算并补齐——
    /// 名称列宽 = 各行显示宽最大值;旧 `chars().count()` 口径下「数据目录」
    /// 按字符数 4 参与列宽、Rust fill 又按字符补空格,表格会错位。
    #[test]
    fn render_listing_aligns_cjk_and_emoji_names_by_display_width() {
        let lines = render_listing(&json!({
            "list": [
                { "Name": "数据目录" },
                { "Name": "a🌍" },
                { "Name": "abc.txt" }
            ]
        }));
        assert_eq!(
            lines,
            [
                "NAME      TYPE  SIZE  MODIFIED",
                "abc.txt   file     -  -",
                "a🌍       file     -  -",
                "数据目录  file     -  -",
            ],
            "CJK/emoji 名称按显示宽(8)补齐,列随 TYPE 对齐"
        );
        // 逐行复核:TYPE 列起点显示位 = 名称列宽 + 两列间隔(字节索引落在
        // ASCII 边界,`display_width` 对前缀切片求值良定义)。
        for line in &lines[1..] {
            let type_pos = line.find("file").expect("type column present");
            assert_eq!(
                display_width(&line[..type_pos]),
                10,
                "名称列(8)+ 间隔(2)按显示宽对齐"
            );
        }
    }
}
