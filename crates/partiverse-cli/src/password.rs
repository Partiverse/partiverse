//! 交互密码读取(M1-WP06-T05 卡内 ④ D7 修复):stdin **关回显**逐行读取,
//! 密码零经 argv(不进 shell history / ps)。unix = nix termios 关 ECHO
//! (`nix` = 树内既有依赖,engine ADR-0005 同版 0.31.3,`term` 特性 = termios
//! 安全封装,nix 0.31.3 特性表实查零新增包/零新增版本;workspace forbid unsafe
//! 下唯一无 unsafe 的回显关断面)。非 unix 平台无 std 级关回显面(Windows 控制台
//! API 需 FFI/unsafe)→ 结构化 Fatal 显式上浮,提示走 `--pass` 脚本路径——
//! **取舍声明**:禁静默「开着回显读」降级(凭据红线,回显 = 终端缓冲泄露面)。
//!
//! 行为口径:①tty 探测经 `tcgetattr`——`ENOTTY`(管道/重定向)时回显本就
//! 无意义,直接裸读(mock stdin 测试缝与脚本管道路径同源);其余探测失败结构化
//! 上浮。②关回显读完后**所有路径**先恢复终端状态再上浮结果(恢复失败优先上浮,
//! 零吞错)。③行尾仅剥 `\n`/`\r\n`,密码本体零改动(内嵌/前导空格合法);
//! 空输入 = 结构化拒绝。④已知局限(卡内声明):读取中 SIGINT 走进程默认处置,
//! 终端回显状态依赖进程退出由 tty 驱动复位;信号级恢复面属 V2 打磨,不在本卡。

use partiverse_core::error::{PartisyError, Severity};

/// 结构化 Fatal 构造(fsops.rs 同款先例:std io 错误为 source 根因)。
fn fatal(message: impl Into<String>) -> PartisyError {
    PartisyError::with_source(
        Severity::Fatal,
        Box::new(std::io::Error::other(message.into())),
    )
}

/// 从生产 stdin 关回显读取一行密码(unix 按终端状态关 ECHO;非 tty 裸读)。
///
/// # Errors
/// 终端探测失败/回显恢复失败/读取失败/空输入 → 结构化 Fatal(severity 保真)。
#[cfg(unix)]
pub fn read_hidden() -> Result<String, PartisyError> {
    use nix::sys::termios::{LocalFlags, SetArg, Termios, tcgetattr, tcsetattr};
    let stdin = std::io::stdin();
    // tty 探测:ENOTTY = 管道/重定向(mock stdin 与脚本管道同路径,裸读);
    // 其余探测失败 = 结构化上浮(零吞错)。
    let original: Option<Termios> = match tcgetattr(&stdin) {
        Ok(saved) => {
            let mut hidden = saved.clone();
            hidden.local_flags.remove(LocalFlags::ECHO);
            tcsetattr(&stdin, SetArg::TCSANOW, &hidden)
                .map_err(|err| fatal(format!("disable terminal echo: {err}")))?;
            Some(saved)
        }
        Err(nix::errno::Errno::ENOTTY) => None,
        Err(err) => return Err(fatal(format!("probe terminal state: {err}"))),
    };
    let result = read_hidden_line(&mut stdin.lock());
    if let Some(saved) = original {
        // 恢复先行:读取结果上浮前必须复原终端(恢复失败优先上浮,零吞错)。
        tcsetattr(&stdin, SetArg::TCSANOW, &saved)
            .map_err(|err| fatal(format!("restore terminal echo: {err}")))?;
    }
    result
}

/// 非 unix 平台:无 std 级关回显面 → 显式 Fatal(取舍见模块文档,禁回显降级)。
#[cfg(not(unix))]
pub fn read_hidden() -> Result<String, PartisyError> {
    Err(fatal(
        "interactive password entry requires a unix terminal; use --pass instead \
         (password will be visible in shell history)",
    ))
}

/// 密码行读取核心(mock stdin 注入缝,单测直驱):读一行,仅剥行尾 `\n`/
/// `\r\n`(密码本体零改动),空输入结构化拒绝。与回显关断面解耦,termios
/// 行为由真机验证(离线无 tty 环境可断言面 = 本函数)。
///
/// # Errors
/// 读取失败 / 空输入 → 结构化 Fatal。
pub fn read_hidden_line(reader: &mut impl std::io::BufRead) -> Result<String, PartisyError> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|err| fatal(format!("read password: {err}")))?;
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        return Err(fatal(
            "empty password rejected (nothing typed before end of input)",
        ));
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// mock stdin(Cursor 注入缝,卡内 ⑤「交互密码 mock stdin」):行尾剥除、
    /// 密码本体保真、空输入拒绝、读取失败上浮。
    #[test]
    fn hidden_line_strips_newline_only_and_rejects_empty() {
        let read = |input: &str| {
            let mut cursor = std::io::Cursor::new(input.to_owned());
            read_hidden_line(&mut cursor)
        };
        assert_eq!(read("s3cret\n").expect("plain newline"), "s3cret");
        assert_eq!(read("s3cret\r\n").expect("crlf"), "s3cret");
        assert_eq!(read("a b  c\n").expect("inner spaces kept"), "a b  c");
        assert_eq!(read("  padded\n").expect("leading spaces kept"), "  padded");
        assert_eq!(
            read("no-newline-eof").expect("eof without newline"),
            "no-newline-eof"
        );
        for empty in ["", "\n", "\r\n"] {
            let err = read(empty).expect_err("empty input rejected");
            assert_eq!(err.severity, Severity::Fatal, "{empty:?}");
            assert!(err.to_string().contains("empty password"), "{err}");
        }
        // 读取失败结构化上浮(禁吞错)。
        struct FailingReader;
        impl std::io::Read for FailingReader {
            fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("reader failure"))
            }
        }
        let mut failing = std::io::BufReader::new(FailingReader);
        let err = read_hidden_line(&mut failing).expect_err("read failure surfaces");
        assert_eq!(err.severity, Severity::Fatal);
        assert!(err.to_string().contains("reader failure"), "{err}");
    }
}
