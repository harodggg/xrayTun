# binaries/ —— 打包时放 xt-daemon 的地方（**尚未接线**）

这个目录是 **cargo 产物 → `.app` bundle** 的暂存位，和 `tauri.conf.json` 的
`bundle.resources` 配合使用。当前 `tauri.conf.json` **刻意没有**声明这条映射，
原因见下。

## 真机/发布时要做的两件事

1. 把 macOS 上构建出来的 `xt-daemon`（arm64 + x86_64 `lipo` 成 universal）放到这里：

   ```
   apps/desktop/binaries/xt-daemon
   ```

2. 在 `apps/desktop/tauri.conf.json` 的 `bundle` 里加一行 `resources` 映射，
   让打包器把它复制到 `XrayTun Next.app/Contents/Resources/xt-daemon`：

   ```json
   "resources": {
     "binaries/xt-daemon": "xt-daemon"
   }
   ```

   加完之后，`apps/desktop/src/daemon_launch.rs` 的
   `<exe>/../Resources/xt-daemon` 查找分支才会命中。

## 为什么现在不声明

`bundle.resources` 里指向一个**不存在的文件**会让 `tauri build` 直接失败。
本仓库现在还没有 macOS 构建产物（也没有 macOS 工具链），把映射写上去等于把一个
必然失败的构建配置提交进去。所以：映射留到 S4 的打包 job 里和二进制一起加，
在那之前用 `XT_DAEMON_BIN=/绝对/路径/xt-daemon` 指向任意已构建的 daemon 即可。

## 查找顺序（代码事实，见 `src/daemon_launch.rs::resolve_binary`）

1. `$XT_DAEMON_BIN`（显式覆盖；指了但不存在只警告，继续往下找）
2. `<主程序目录>/../Resources/xt-daemon` —— macOS bundle 形态
3. `<主程序目录>/xt-daemon` —— `cargo build` 开发形态（两个二进制同在 `target/debug/`）

## 这个目录下不该有的东西

* 不要提交**假的** `xt-daemon`（空文件、脚本、占位可执行）—— 它会被原样打进 bundle，
  然后在真机上以「spawn 成功但立刻退出」的形式变成最难查的一类故障。
* 不要提交 xray 核心、geoip/geosite `.dat`：它们归 daemon/数据面，不属于壳。
