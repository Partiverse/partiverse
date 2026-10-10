# ADR-0009 WP06 传输校验依赖集(sha2 0.11 复用 + proptest devDep;blake3 否决)

- 状态: 接受 | 日期: 2026-10-11 | 负责人: @partiverse
- 背景: M1-WP06-T03(哈希比对降级链+两段采样哈希)。卡内 ③ 给出二选一:引 blake3(须本 ADR:license 实测+商业授权核查+deny 全节绿)或 std 手写/树内既有哈希(Owner 预飞已核实「Sha2 0.11 已在树,ADR-0004」)。硬约束:采样哈希为过渡件(复用评估 §2:约 30-50 行+proptest;V2 交 partisync cas,架构 D27 迁移目标),禁引大依赖(digest 系选型最小面,卡内禁止行为)、workspace 禁 unsafe、deny `multiple-versions="deny"` 不可改。
- 决策:
  1. partiverse-core 新增普通依赖 **sha2 0.11**(license=MIT OR Apache-2.0,ADR-0004 已裁定该选型与版本;锁树既有 partiverse-engine 同版 0.11.0,**零新增包/零新增版本**,仅新增依赖边)。采样指纹 = SHA-256(域分隔串 `partiverse-sample-v1` ‖ size_be_u64 ‖ 头64KiB ‖ 尾64KiB),标注格式 `pv-sample-sha256:<hex>`。
  2. dev-dependencies 新增 **proptest 1.x**(license=MIT OR Apache-2.0,registry 实测;卡内 ⑤ 点名要求)——本 ADR 下唯一新增锁树面,传递依赖(proptest 自身树:rand/getrandom 老线、bit-set/bit-vec、regex-syntax、unarray、num-traits 等)以 `cargo deny check` 四节实测绿收口。
  3. **blake3 否决**(候选池另一项,逐条理由):
     - 新增顶层依赖 + 新版本包,违反「digest 系选型最小面」;其默认构建走 C/asm(cc 构建链,`c`/`asm` 特性)或纯 Rust 降级(`no_avx512` 等特性面),引入本仓首个 C 构建依赖面,收益为零。
     - 采样哈希是过渡件,V2 整体移交 partisync cas(D27);blake3 的性能优势(并行/长输入)在「头64KiB+尾64KiB」≤128KiB 输入上不可见。
     - sha2 0.11 已被 ADR-0004 裁定入树,零新增版本;采样指纹在本卡语义是**变更检测指纹**(非长期内容承诺),算法品牌无外部兼容约束。
- 商业授权核查: sha2/proptest 均 MIT OR Apache-2.0 双授权,无付费许可/年费/审核费/主体证书条款;商业授权审计表无需新增行(审计表只收录含商业授权风险的依赖)。
- 证据(全部本机 2026-10-11 实测,非记忆):`cargo tree -i sha2@0.11.0` = partiverse-engine [dev-dependencies] 单边;`grep proptest Cargo.lock` 加依赖前零命中;`cargo deny check` 加依赖后四节绿(bans/licenses/sources/advisories);`cargo tree -p partiverse-core` 确认 sha2 进普通依赖、proptest 仅 dev。
- 影响: 正向=校验链零新增版本即得密码学原语;性质测试有卡点名框架。代价=①proptest 传递树进 dev 锁面(仅测试目标,不进生产二进制);②sha2 升级跟随 RustCrypto 0.11 线;③采样指纹算法前缀 `partiverse-sample-v1` 一经落库即冻结,后续改算法须换前缀版本号。
