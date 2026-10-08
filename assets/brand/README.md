# Placeholder Brand Assets(占位品牌资产)

> ⚠️ 本目录全部为 **PLACEHOLDER**(✅D28):确定性生成、仅供开发;**正式 logo/图标设计完成后必须整体替换**,替换前不得公开发布。

## 内容

- `placeholder.svg`:应用占位标识("P" 字标 + 品牌占位色 `#5B8DEF`),所有尺寸的生成源;
- 尺寸规范(自 placeholder.svg 导出):`icon_16/32/48/128/256/512/1024.png` + `icon.icns`(macOS)+ `icon.ico`(Windows)——生成脚本在 WP10 落地(`scripts/gen-icons.*`),当前仅提供 SVG 源。

## 纪律

1. 任何代码/配置引用图标一律经 `assets/brand/` 路径,**禁止**把位图内联进源码;
2. 品牌色只存在于 `tokens.json`(`brand.primary` 等),换正式色=改 token,禁止 hex 写死;
3. 显示名公开渠道固定 "Partiverse"(产品中文名保密纪律见 AGENTS.md);
4. 发布 gate:REPLACE-BEFORE-RELEASE.md 全部勾选 + 商标检索通过(D5/D11)后才允许正式资产入库。
