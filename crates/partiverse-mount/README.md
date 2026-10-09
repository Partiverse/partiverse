# partiverse-mount

rclone mount/nfsmount 包装(挂载面)。V1 不含挂载(挂载为系统盘是 V2 交付,✅D16);
V2 需覆盖 Windows 引导安装 WinFsp、macOS nfsmount(FUSE-T 引导)、Linux FUSE 直用,
架构见 `docs/06-架构设计-Phase0草案.md` §7。

**占位状态**:M1-WP01-T01 仅建空壳(lib.rs 无实现),功能由 V2 对应 WP 落地。
