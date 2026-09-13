# binaries/

把 Xray 核心放在这里（用 `scripts/fetch-xray.sh` 下载）。

```
apps/desktop/binaries/
  xray            # 唯一的可执行文件
  geoip.dat       # geoip:cn 等规则依赖它
  geosite.dat     # geosite:cn 等规则依赖它
  LICENSE-xray
```

## 为什么 `.dat` 不是可选的

配置里的 `geoip:cn` / `geosite:cn` 会在**运行期**去读这两个文件。
缺失时的行为是「规则不命中」，也就是**静默地不分流** —— 日志里没有任何错误，
用户只会看到「绕过大陆」预设完全没起作用。所以打包时必须三个文件一起带上。

## 目前的位置解析顺序

`xt_core::xray::resolve_core_binary` 依次查找：

1. 用户在「设置 → 内核」里指定的绝对路径
2. `Contents/Resources/xray`（发行版形态）
3. `Contents/Resources/bin/xray`
4. 与主程序同级（`cargo run` 开发形态）
5. `/opt/homebrew/bin/xray`、`/usr/local/bin/xray`、`/usr/bin/xray`
6. `PATH`

绝对路径里没有这一层 `binaries/`，因为它是**打包时的暂存目录** ——
`scripts/fetch-xray.sh` 把它当作下载目标，打包脚本再从这里复制进 app bundle。

`.dat` 文件的部署尚未接线，见 `docs/07-roadmap-and-risks.md` 的未完成项第 1 条。
