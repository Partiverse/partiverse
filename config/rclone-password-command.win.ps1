# Partiverse rclone config 主密钥取密脚本(Windows,模板)—— M1-WP04-T01。
# 经 rclone 官方 RCLONE_PASSWORD_COMMAND 注入并直接执行,向 stdout 输出主
# 密钥;密钥零 argv、零落盘、零日志。存储面:Windows Credential Manager
# (keyring 2.3.3 windows 后端:Generic Credential,target_name =
# `<account>.<service>`,blob = UTF-16LE,见其 src/windows.rs)。
# **待 Owner 实测**(CI 仅编译门,ADR-0007);service/account 与编译期常量一致。

$TargetName = 'rclone-config.partiverse'

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class CredMan {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    public struct CREDENTIAL {
        public int Flags;
        public int Type;
        public string TargetName;
        public string Comment;
        public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
        public int CredentialBlobSize;
        public IntPtr CredentialBlob;
        public int Persist;
        public int AttributeCount;
        public IntPtr Attributes;
        public string TargetAlias;
        public string UserName;
    }
    [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern bool CredReadW(string target, int type, int flags, out IntPtr credPtr);
    [DllImport("advapi32.dll")]
    public static extern void CredFree(IntPtr cred);
}
'@

$credPtr = [IntPtr]::Zero
try {
    # CRED_TYPE_GENERIC = 1(keyring windows 后端写入类型,src/windows.rs)。
    if (-not [CredMan]::CredReadW($TargetName, 1, 0, [ref]$credPtr)) {
        [Console]::Error.WriteLine("partiverse: credential '$TargetName' not readable")
        exit 1
    }
    $cred = [Runtime.InteropServices.Marshal]::PtrToStructure($credPtr, [type][CredMan+CREDENTIAL])
    $blob = New-Object byte[] $cred.CredentialBlobSize
    [Runtime.InteropServices.Marshal]::Copy($cred.CredentialBlob, $blob, 0, $blob.Size)
    # blob = UTF-16LE(keyring windows 后端编码),解码输出。
    [Console]::Out.Write([Text.Encoding]::Unicode.GetString($blob))
} finally {
    if ($credPtr -ne [IntPtr]::Zero) { [CredMan]::CredFree($credPtr) }
}
