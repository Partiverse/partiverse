# partiverse-engine

rclone rcd 引擎协调器:rcd 生命周期(spawn/探活/优雅退出/崩溃重启)、unix socket
控制面接入、引擎热更新(下载校验/原子切换/回滚)。架构见
`docs/06-架构设计-Phase0草案.md` §3/§4。

**占位状态**:M1-WP01-T01 仅建空壳(lib.rs 无实现),功能由 WP02(引擎协调器)落地。
