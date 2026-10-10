#!/bin/sh
# Partiverse rclone config 主密钥取密脚本(unix,模板)—— M1-WP04-T01。
# 经 rclone 官方 RCLONE_PASSWORD_COMMAND 注入(其以 SpaceSepList 直接 exec
# 本脚本、不经 shell,1.75.1 实机核实);从 OS keychain 取主密钥打印到
# stdout,密钥零 argv、零落盘、零日志。存储面=linux 内核 keyutils(keyring
# 2.3.3 后端:会话 keyring @s,description=keyring-rs:<account>@<service>)。
# 语义(ADR-0007):内核 keyring 不跨重启(「secure cache」),重启后取密
# 失败 → rclone 报解密失败 → 上层显式重新供给。
# 部署:随应用分发并授予执行位;service/account 与编译期常量一致
# (credential_store::KEYRING_SERVICE/KEYRING_ACCOUNT)。

DESC='keyring-rs:rclone-config@partiverse'
ID=$(keyctl search @s user "$DESC") || exit 1
exec keyctl pipe "$ID"
