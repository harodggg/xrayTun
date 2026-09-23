# v0.8.37 发布验收（热修：`duplicate ruleTag` 启动阻断）——两阶段，机制第二次真跑

> 口径：所有数字都是**时点值**；「Lead 实测」「tester 实测」「我（ops）实测」分开写；
> 未验证/未结论的项一律进 §13 诚实清单。

## 1. 冻结链与两阶段

| 项 | 值 |
|---|---|
| **冻结候选**（Lead 给的确切哈希） | `6db2791b89cfb5a77b67d5bc9b2d156e7b3dcca0` |
| **提交 1**（= 门禁目标 = **tag 目标**） | `ef768fa4863c1492edc9b708b769cd3d0be25a47` |
| **annotated tag `v0.8.37`** | tag 对象 `06d8f456fda7e2a06dd3ea696cbbc69b69ce8264`，tagger epoch **1790166452**（2026-09-23 20:27:32 +0800） |
| **提交 2** | `4b20e06670688a8d5537a3376dbf973cc295dab5` |
| 冻结时的范围 `v0.8.36..6db2791` | **7 个提交**：`ac82111`(P0) / `ee96609` + `2e5e588`(守卫第二轮) / `6db2791`(UI 防呆) + `c25a0d5` + `f0931c6` + `a4b99af`（文档） |
| 打 tag 时的工作树 | 干净；`HEAD == origin/main == ef768fa` |

```
$ git cat-file -p v0.8.37 | head -5
object ef768fa4863c1492edc9b708b769cd3d0be25a47
type commit
tag v0.8.37
tagger harodggg <haroldtiansheng@gmail.com> 1790166452 +0800

$ git ls-remote --tags origin v0.8.37
06d8f456fda7e2a06dd3ea696cbbc69b69ce8264	refs/tags/v0.8.37
```
⚠️ **推 tag 的第一次尝试失败**（`Connection reset by 20.205.243.166 port 22`），
**第二次成功**（`* [new tag] v0.8.37 -> v0.8.37`）。两次都留在这里：网络抖动，不是权限或 tag 内容问题。

## 2. 改前基线（提交 1 之后 / tag 之前的工作树）

口径：`size` = `stat -f %z`（字节）；`mtime` = `stat -f '%Sm' -t '%Y-%m-%d %H:%M:%S'`；
**时点 = 2026-09-23 20:31:42 +0800**，`HEAD = ef768fa`。

```
Cargo.toml                         1570  2026-09-23 20:13:43
Cargo.lock                       119448  2026-09-23 20:13:43
apps/desktop/tauri.conf.json       1816  2026-09-23 20:13:43
apps/ui/package.json                753  2026-09-23 20:13:43
scripts/gen-site-jsonld.py        18724  2026-09-23 20:13:43
scripts/gen-site-geo.py           23315  2026-09-23 20:13:43
scripts/gen-site-images.py        11933  2026-09-23 20:13:43
site/assets/site.js                4878  2026-09-23 20:13:43
site/index.html                   43696  2026-09-23 20:13:43
site/en/index.html                44810  2026-09-23 20:13:43
site/llms.txt                      4851  2026-09-23 20:13:43
site/llms-full.txt                63027  2026-09-23 20:13:43
CHANGELOG.md                     120419  2026-09-23 20:13:36
og: 59209 site/og-image-0.8.37.png / 42477 site/og-image-en-0.8.37.png
v0.8.36..HEAD 提交数 = 11
磁盘: /dev/disk3s5   482797652 428817424  19832380    96% ...   /System/Volumes/Data
```

## 3. 机制第二次真跑：`bump-release.py phase1`（**在冻结哈希上重跑**）

```
$ python3 scripts/bump-release.py phase1 --new 0.8.37 --date 2026-09-23 --dry-run
[phase1 干跑] 0.8.36 → 0.8.37（仓库 /Users/xbtg-/deepseek-harness/xray-tun）
  …42 条规则逐条 ✓（命中次数全部等于预期）…
✓ 全部 42 条校验通过（顺序模拟）；干跑，不落盘。将写 12 个文件：
    Cargo.toml / apps/desktop/tauri.conf.json / apps/ui/package.json / scripts/gen-site-{jsonld,geo,images}.py /
    site/assets/site.js / site/wasm/index.html / site/en/wasm/index.html / Cargo.lock /
    site/index.html / site/en/index.html
[旧 og 卡片] site/** 里对 0.8.36 的引用 = 0（必须 0）
  （干跑）会删 site/og-image-0.8.36.png 与 site/og-image-en-0.8.36.png
（干跑后：Cargo*/site/版本来源 上 `git status --porcelain` **为空**）

$ python3 scripts/bump-release.py phase1 --new 0.8.37 --date 2026-09-23      # 落盘
✓ 全部 42 条校验通过（顺序模拟）；已写 12 个文件
[旧 og 卡片] site/** 里对 0.8.36 的引用 = 0（必须 0）
  删除 site/og-image-0.8.36.png 与 site/og-image-en-0.8.36.png
```
`✓` 行 **43 = 42 条规则 + 1 条引用判据**；**「切引用」与「删旧 og 卡片」在同一批**
（v0.8.35 那次线上 og 404 窗口的根因在机制上消除）。随后手工跑三个生成器：
JSON-LD `gen` + `check` 全过、geo 写出 4 个索引产物、新 og **59,209 B / 42,477 B**。

## 4. 三条 0 判据 + 开关（提交 1 实测）

| 判据 | 实测 |
|---|---|
| `site/**` 里 pinned 直链 `releases/download/v0.8.37` | **0** |
| 上一版**真实**字节数 `47,446,724` / `42,936,627` | **0 / 0** |
| `site/**` 里 `0.8.36` 残留 | **0** |
| `PUBLISHED`（jsonld / geo） | `False` / `False` |
| 「正在发布」/ `publishing now` | 4 / 2 |
| 六条站点版本断言 + `Cargo.toml` + 两个 wasm 页（手工核） | 全部 `0.8.37` |
| 部署静态检查本地复刻 | **fail=0** |

## 5. 门禁

**Lead 实测（tag 目标 `ef768fa`）**：`BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build`
⇒ **`GATE_EXIT=0`**，窗口 **20:15:13–20:27:18**：

```
Test Files  37 passed (37)
     Tests  366 passed | 1 todo (367)        ← **无 Errors 行**
✓ 站点声明的版本与 Cargo.toml 一致：0.8.37
✓ 与 CI 相同的全部检查通过
```
⚠️ **同一提交第一次为 `GATE_EXIT=75`**：strict 判据把 tester 一个**命令行文本里含 `cargo` 字样**的
后台包装进程当成「未持锁的 cargo」。**75 = 环境问题、不是代码失败**；已进 `task-162`
（strict 判据不该用 `pgrep -f` 匹配命令行文本）与 `GUARD-FALSE-GREEN-PATTERNS.md` 的 **R7**。
⚠️ **看退出码 + `Errors` 行，不看通过数**：`366 passed` 这次是绿的，同样的数量级在历史上红过。

## 6. helper 侧「不需要重装」——生产区对照（口径：第一个顶层 `#[cfg(test)]` 之前 = 生产区）

| 文件（`v0.8.36..6db2791`） | 生产区 |
|---|---|
| `crates/xt-proto` | diff **空** ⇒ 协议号未变 ⇒ 设置页不会报「版本不匹配」 |
| `xt-tun/src/macos/controller.rs` | 374 → 374 行，**逐行相同** |
| `xt-tun/src/macos/mod.rs` | 62 → 62 行，**逐行相同** |
| `xt-tun/src/macos/snapshot.rs` | 35 → 35 行，**逐行相同** |
| `xt-helper/src/server.rs` | 821 → **848 行（生产区 +46）** —— 结构重构，见下 |

`server.rs` 的等价性判断：`fn uninstall` 变成**一行委托** `self.uninstall_with(...)`；
`uninstall_with` 内是 `uninstall_outcome(session_rollback(), force_cleanup())` → `warn!` → `remove_traces` → 返回；
**Rust 实参左到右求值** ⇒ 顺序与旧写法一致（内存那份会话 → `force_cleanup`）；文案字面量、
`warn!` 时机、副作用顺序（launchctl → socket → 已安装标记）逐项相同。

⇒ **没有行为修复**（无用户可见行为变化、协议号未变）⇒ **本版不需要重装助手**；
已为 v0.8.35 重装过的不必再装，**还没为 v0.8.35 装过的请补装一次**（那次的行为修复在助手侧）。

### 6.1 真实核心验证的**判据分层**与自检文案长度归因（tester 的 `task-166`，`c25a0d5`）

| 撤掉哪一层 | 实测结果 |
|---|---|
| 只撤**第一层**（`merge_rules`，预设 + 自定义两分支） | 最终配置**仍唯一**、真实核心**仍 exit 0**；第一层由**一组单测**直接断言其输出 |
| 只撤**第二层**（`build_routing` 的最终 `rules` 数组） | **三个内部入口复现 exit 23**（`duplicate ruleTag internal-api` 等）—— 只有这层看得见 `internal-dns-hijack` / `internal-api` / `internal-fallback` |
| **两层都撤**（= 修前） | 用户形态复现 **exit 23**，与用户原文**逐字一致**：`duplicate ruleTag preset-private` |
| 修后（两层都在） | 10 份配置 `xray run -test -c` **全部 exit 0** |

⇒ **最终产物（生成配置）的唯一性由第二层保证**；第一层是更早的收口（有自己的单测）。
**不许**写成「撤掉第一层真实核心就会红」—— 实测不会。

**自检文案长度归因**：修前 **3,468 字符 / 35 行**，**主因是用户的 `log_level=debug`**
（生成的配置带 `loglevel=debug` ⇒ 含 **10 行 `[Debug]`**；用 `warning` 级对照仅 **4 行 / 382 B 量级**）；
**不是**两种调用形式造成的。修后 **522 字符 / 9 行**
（结论 + 指名 tag + 次数 + 来源 + 下一步 + 末尾 8 行）。

## 7. 真实资产（`gh release view v0.8.37 --json isDraft,assets`）

```
{"tagName":"v0.8.37","isDraft":false,"isPrerelease":false,"publishedAt":"2026-09-23T12:54:03Z",
 "assets":[
  {"name":"SHA256SUMS.txt","size":200,"state":"uploaded",
   "digest":"sha256:40a402c7bde532b359317a1f64a8c7b3f201391cc0faba1131827ff2d62b306d"},
  {"name":"XrayTun_0.8.37_x86_64_arm64.dmg","size":47469092,"state":"uploaded",
   "digest":"sha256:ea20d5d11808a7ed91fa5da59070107d99e3b380a92f547895b24e8a1e700113"},
  {"name":"XrayTun_0.8.37_x86_64_arm64.zip","size":42954264,"state":"uploaded",
   "digest":"sha256:ce4ec9c1edba5fb22280ed52a454ad3e71c46d287a84c2e4539f7bab82d3bbe7"}]}
```
**Release workflow**：run `35860683951`（tag `v0.8.37`）⇒ **success**。

**三方一致**：

| 三方 | SHA256SUMS.txt | dmg | zip |
|---|---|---|---|
| GitHub `assets[].digest` | `40a402c7…306d` | `ea20d5d1…0113` | `ce4ec9c1…bbe7` |
| 我本地 `shasum -a 256`（下载后） | `40a402c7…306d` ✓ | （未下载，两方互证） | `ce4ec9c1…bbe7` ✓ |
| 发布资产内 `SHA256SUMS.txt` 原文 | — | `ea20d5d1…0113` ✓ | `ce4ec9c1…bbe7` ✓ |

⚠️ **下载第一次失败**（`gh release download`：SHA256SUMS.txt 报
`read: connection reset by peer`，zip 只落了 146,753 B）⇒ **第二次重试成功**，
落盘尺寸 `zip = 42,954,264`（吻合）与 `SHA256SUMS.txt = 200`。两次都记在这里。
⚠️ dmg 的 47 MB **我没有下载复算** —— 它的 SHA 是「GitHub digest ↔ 发布资产里 `SHA256SUMS.txt` 那一行」
**两方互证**，证据强度与 zip 不同，别混着引。

## 8. 提交 2（真实字节 + `PUBLISHED=True` + pinned + 生成器重跑）

```
$ python3 scripts/bump-release.py phase2 --new 0.8.37 --dmg-bytes 47469092 --zip-bytes 42954264 \
      --sha-bytes 200 --date 2026-09-23
✓ 全部 26 条校验通过（顺序模拟）；已写 4 个文件：
    scripts/gen-site-jsonld.py / scripts/gen-site-geo.py / site/index.html / site/en/index.html
```
MiB 口径 = **ROUND_HALF_UP 一位小数**（工具算、不手抄）：
`47,469,092 / 1,048,576 = 45.270054 → **45.3**`、`42,954,264 / 1,048,576 = 40.964378 → **41.0**`。
（⚠️ 与上一版**口径相同但取值不同**：不是 45.2/40.9 —— 手抄必错。）

| 判据 | 实测 |
|---|---|
| pinned `releases/download/v0.8.37` | **30**（两页各 8 = 6 条 HTML + JSON-LD 2） |
| 「正在发布」/ `publishing now` | **0 / 0** |
| 旧真值 `47,446,724` / `42,936,627` | **0 / 0** |
| 本版真值 `47,469,092` / `42,954,264` | **5 / 5**；`45.3 MiB` 17 处、`41.0 MiB` 9 处 |
| `PUBLISHED` / `LAST_PUB` | `True` ×2 / `"2026-09-23"` |
| 部署静态检查本地复刻 | **fail=0** |
| 幂等性 | 已发布态再跑 `phase2` ⇒ 非 0（设计如此） |

与上一版对照：dmg 47,446,724 → **47,469,092**（+22,368）、zip 42,936,627 → **42,954,264**（+17,637）。

## 9. 线上验收

```
$ VER=0.8.37 PREV=0.8.36 ./scripts/verify-live-site.sh          → LIVE_EXIT=0
总结：全部通过（1 条 warning）
⚠️ WARNING: curl 连不上 releases/latest（HTTP 000）—— **网络/DNS 问题，不是「latest 指向错」**，这一项本次没结论
$ ./scripts/verify-live-site.sh --self-test                      → SELFTEST_EXIT=0
self-test：四例全部符合预期（异常样例 ✗ / 缓存残留只 WARN / 真 404 正常）
```
（部署：`Pages` 与 `Cloudflare Pages` 对提交 2 `4b20e06` 都是 **completed/success**。）

**我自己 curl 的复核（时点 2026-09-23 23:55:59 +0800）**：

```
zh 页 43980 B sha256 68bb8f4d…  pinned=8 「正在发布」=0  0.8.36 残留=0
     真实字节在页面上：47,469,092 ×1 · 42,954,264 ×1 · 45.3 MiB ×4 · 41.0 MiB ×2
en 页 pinned=8  publishing=0  0.8.36=0  「47,469,092 bytes」×1
cmp：zh 线上 == 仓库 ✓ ；en 线上 == 仓库 ✓
og 图：og-image-0.8.37.png 200 / 59,209 B / sha256 7df416c3… == 仓库 ✓
      og-image-en-0.8.37.png 200 / 42,477 B / sha256 71640069… == 仓库 ✓
```
**未结论项**：`releases/latest` 那一项因网络（HTTP 000）**本次没拿到结论**；
我另外重试了 2 次仍是 `000`（github.com 今天多次连接重置），**不写「通过」也不写「失败」**。

## 10. `.app` 三脚本（**发布资产 zip**）+ 签名

```
$ ditto -x -k XrayTun_0.8.37_x86_64_arm64.zip /tmp/v0837-app
$ ls -l .../XrayTun.app/Contents/Resources/scripts/
  incident-bundle.sh  36,701 B   triage-incident.py  45,288 B   net-metrics.py  49,932 B
$ 与 **tag 里的文件**逐个比：
  incident-bundle.sh   tag=7662c2193a9e6bf7 包内=7662c2193a9e6bf7 ⇒ ✓ 逐字节相同
  triage-incident.py   tag=6969e38b5355bab4 包内=6969e38b5355bab4 ⇒ ✓ 逐字节相同
  net-metrics.py       tag=e45318da088cb16f 包内=e45318da088cb16f ⇒ ✓ 逐字节相同
$ codesign --verify --strict --verbose=2
  /tmp/v0837-app/XrayTun.app: valid on disk
  /tmp/v0837-app/XrayTun.app: satisfies its Designated Requirement      （exit 0）
```

⚠️ **口径事故（如实记）**：我第一遍用 `docs/verification/verify-app-bundle-resources.sh`
直接跑，它报 **2 红**（`incident-bundle.sh` / `triage-incident.py` 与仓库不一致）——**是假红**：
该脚本拿**当前工作树**比，而工作树在 tag 之后被 `96df259`（tester 的 `task-171`）改过那两个脚本。
**正确口径是「与 tag/发布提交里的文件比」**。按 tag 比 ⇒ 三个都逐字节相同（上表）。
（改进项：给该脚本加 `--rev <tag>`，别拿移动中的工作树当基准；不在本卡写入范围，已报 Lead。）

## 11. Release Notes 机制第二次真跑

```
$ python3 scripts/bump-release.py notes --tag v0.8.37 --out /tmp/v0837-notes-local.md
[notes] 动作来源 docs/release-notes/v0.8.37.md（4347 字节，有必须的用户动作）
[notes] 写出 /tmp/v0837-notes-local.md（7243 字节）
$ gh release view v0.8.37 --json body --jq '.body' > /tmp/v0837-body-ci.md    # 7244 字节
$ cmp <(cat 本地; printf '\n') 读回
✓ **逐字节相同**
关键结构：动作段在最前（`> ## 本版修一个**启动阻断**…`），第 57 行 `---` 之后是固定模板（`## 安装` ×1）
```
**尾换行差异如实写**：本地 7,243 B（末尾 `privileges.md\`。\n`）→ CI 读回 7,244 B（`…\n\n`）——
**GitHub 规范化为正文补一个尾换行**；`本地 + "\n" == 读回` 为真，**不是丢字**。
⇒ 机制第二次生效：**本版同样不需要发布后手工 `gh release edit`**。

## 12. 本版对照基线（`net-metrics.py`，带口径）

```
$ python3 scripts/net-metrics.py --until "2026-09-23 20:27:32"      # = tagger epoch 1790166452
N = 窗口内命中 389,666 条（移动标记：日志在被持续追加）；T = 2026-09-23 20:27:32（移动标记）
日志：app.1.jsonl = 68,778,150 B / mtime 20:27:41；app.jsonl = 19,133,243 B / mtime 20:38:56
切：全部 → T ；解：全文件扫描 **440,423 行 → 440,423 个对象**（多对象行 0、坏行 0、message 含 LF 0）
匹配：键 (ts_unix, message) 先见者胜、跨文件 app.1 → app；丢弃重复 **3 条**（窗口内 0）
来源：source=app 1,263 / source=core 388,403 / 其它 0
选择内容指纹：sha256:f9e5d3780ef0d984d7ce687240384ca5517a3c0b58c5b5559167c1558b4fabdd
```

| 指标 | 值（同一窗口） |
|---|---|
| `replace destination with tcp:[240e…` | **0**（v0.8.34 修的 v6 改写，仍为 0） |
| `failed to open connection` | **0** |
| 有改写的连接（按 session id 归并） | 2,780（全 v4：失败 **0**，0.0%） |
| 探针连接 / 成功 / 失败 | **966 / 966 / 0**（探针轮 961，含失败的轮 0） |
| `cp.cloudflare.com:80` 成功耗时 | 中位 133.5ms、p90 619.6、p95 759.5、p99 2644.6、max 22599.5（n=966） |
| `>6s 才成功` | **3 条**（v0.8.34 把探针超时放宽到 10s 的依据） |
| 「已作废」/「隧道已自动恢复」 | **1 / 0**（口径A：16:17:46 → 16:18:36，50 s；口径B 52 s） |
| `core 启动` | 6 次；「物理出口已变化」0 次 |

完整输出 46 行：`/tmp/v0837-netmetrics.txt`。

## 13. 诚实清单

* **提交信息的显式更正（不重写已推送历史，以本更正为准）**：
  `ee96609` 的提交信息里有一句不实：「新增接缝（`uninstall_with` / `TestExecutor` / `with_test_root`）
  **只在 `#[cfg(test)]` 下存在**」—— 实际 `uninstall_with` 是**生产函数**（生产路径调用它）；
  只有**注入入口**是测试专用。订正落在 `docs/verification/TASK-163-SEAM-CORRECTION.md`（提交 `f0931c6`）。
  **结论不变：本版不需要重装助手。**（本条保留在文档里，不悄悄修好。）
* **`releases/latest` 本次没有结论**：curl 返回 HTTP 000（网络/DNS；我今天还遇到 tag 推送与资产下载
  各一次 `connection reset`）⇒ 不写「通过」也不写「失败」；
* **`.app` 校验第一遍是假红**（拿移动中的工作树当基准）⇒ 正确口径是「与 tag 里的文件比」，
  按 tag 比三个脚本都逐字节相同；改进项见 §10；
* **dmg 未下载复算**（两方互证，证据强度低于 zip）；
* **真机「App 里切预设 → 启动」这条 UI 路径未验证**；验到的是「生成路径 + 真实核心 `xray run -test`」；
* 唯一化只在**生成配置时**发生，**不改用户磁盘上的 `settings.json`**（tester 实测 sha256 未变）；
* 真机安装 / 重启 / Gatekeeper / 重装助手未验证；本环境不在中国大陆，GFW 行为无法复现；
* 用户装上新版后的「after」数字**不存在**，不预填；§12 的数字是**发版前的基线**且带**移动标记**；
* §6 的生产区对照是**文本/结构层**证据（L2），**不是行为证明**（L1 = backend-dev 的站点级行为测试
  + tester 的 `task-166` 真实核心验证）；我不拿 L2 冒充 L1；
* 门禁 strict 判据当前**过宽**（`task-162`）：**75 = 环境问题**（别人的 cargo / 命令行里含 `cargo` 字样的进程），
  不是代码失败；本版两次实例（一次是本提交的第一次运行）；
* `check.sh` 的 6 条站点断言**不覆盖** `Cargo.lock` 与 `site/{,en/}wasm/index.html`（后者**手工核**过）；
* `bump-release.py` 的开关是 `--dry-run`（**默认落盘**），卡面写的 `--apply` 不存在；
  改进项（默认干跑 + 显式 `--apply`）并入 `task-147` 后续，本版**没有**在冻结窗口改工具。
