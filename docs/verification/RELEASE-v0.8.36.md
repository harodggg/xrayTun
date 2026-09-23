# v0.8.36 发布验收（两阶段；新发版机制第一次真跑）

> **状态：提交 2 阶段**（tag 已推、资产待确认；本文件在提交 2 落盘）。
> 口径：所有数字都是**时点值**；「Lead 实测」与「我（ops）实测」分开写；未验证项进 §14 诚实清单。

## 1. 冻结链与两阶段

| 项 | 值 |
|---|---|
| **冻结候选**（Lead 给的确切哈希） | `941f71df242e86fdd32a112599e4f7b42904abe8` |
| **提交 1**（版本号 8 处 + `PUBLISHED=False` + 站点「正在发布」+ CHANGELOG + 本版动作文件） | `c5435154f67bd1c1e9513e583664bf379a1bdaa9` |
| **tag 目标**（= 提交 1，门禁在它上面跑） | `c5435154f67bd1c1e9513e583664bf379a1bdaa9` |
| **annotated tag `v0.8.36`** | tag 对象 `bf13b9c81b8867afaf8cb82e1f37ab5b53226c73`，tagger epoch **1790158982**（2026-09-23 18:23:02 +0800） |
| **提交 2** | `03cc02f210354d6932992f1ffcc50c205dd3539c` |
| `git rev-list --count v0.8.35..HEAD`（提交 1 前） | **25** |
| 工作树（提交 1 前） | 干净；`origin/main == HEAD` |

```
$ git cat-file -p v0.8.36 | head -5
object c5435154f67bd1c1e9513e583664bf379a1bdaa9
type commit
tag v0.8.36
tagger harodggg <haroldtiansheng@gmail.com> 1790158982 +0800

$ git ls-remote --tags origin v0.8.36
bf13b9c81b8867afaf8cb82e1f37ab5b53226c73	refs/tags/v0.8.36
```

## 2. 改前基线（**提交 1 之后 / tag 之前**的工作树）

口径：`size` = `stat -f %z`（字节）；`mtime` = `stat -f '%Sm' -t '%Y-%m-%d %H:%M:%S'`；
**时点 = 2026-09-23 18:23:32 +0800**。

```
Cargo.toml                         1570  2026-09-23 18:11:55
Cargo.lock                       119448  2026-09-23 18:11:55
apps/desktop/tauri.conf.json       1816  2026-09-23 18:11:55
apps/ui/package.json                753  2026-09-23 18:11:55
scripts/gen-site-jsonld.py        18724  2026-09-23 18:11:55
scripts/gen-site-geo.py           23315  2026-09-23 18:11:55
scripts/gen-site-images.py        11933  2026-09-23 18:11:55
site/assets/site.js                4878  2026-09-23 18:11:55
site/index.html                   43696  2026-09-23 18:11:59
site/en/index.html                44810  2026-09-23 18:11:59
site/llms.txt                      4851  2026-09-23 18:11:59
site/llms-full.txt                63027  2026-09-23 18:11:59
CHANGELOG.md                     110091  2026-09-23 18:11:46
og: 59013 site/og-image-0.8.36.png / 42294 site/og-image-en-0.8.36.png
HEAD=c5435154f67bd1c1e9513e583664bf379a1bdaa9
磁盘: /dev/disk3s5   482797652 422897576  25752288    95% ...   /System/Volumes/Data
```

## 3. 机制第一次真跑：`scripts/bump-release.py phase1`

```
$ python3 scripts/bump-release.py phase1 --new 0.8.36 --date 2026-09-23 --dry-run
  …42 条规则逐条 ✓（命中次数全部等于预期）…
✓ 全部 42 条校验通过（顺序模拟）；干跑，不落盘。将写 12 个文件：
    Cargo.toml / apps/desktop/tauri.conf.json / apps/ui/package.json / scripts/gen-site-{jsonld,geo,images}.py /
    site/assets/site.js / site/wasm/index.html / site/en/wasm/index.html / Cargo.lock / site/index.html / site/en/index.html
[旧 og 卡片] site/** 里对 0.8.35 的引用 = 0（必须 0）
  （干跑）会删 site/og-image-0.8.35.png 与 site/og-image-en-0.8.35.png

$ python3 scripts/bump-release.py phase1 --new 0.8.36 --date 2026-09-23        # 落盘
✓ 全部 42 条校验通过（顺序模拟）；已写 12 个文件
[旧 og 卡片] site/** 里对 0.8.35 的引用 = 0（必须 0）
  删除 site/og-image-0.8.35.png 与 site/og-image-en-0.8.35.png
```

* **「切引用」与「删旧 og 卡片」在同一批** —— v0.8.35 那次线上 og 404 窗口的根因
  （两件事被拆到两个提交）在机制上消除；本条由**同一份工具输出的相邻两段**直接证明；
* 随后手工跑三个生成器：JSON-LD `gen` + `check` 全过、`gen-site-geo.py` 写出 4 个索引产物、
  `gen-site-images.py` 生成新 og（59,013 / 42,294 B）；
* **接口如实说明**：卡面写的 `--apply` **不存在** —— 工具的开关是 `--dry-run`（默认落盘）。
  本次按实际接口执行（先 `--dry-run` 复核，再不带开关落盘）；冻结窗口内**没有**改工具
  （`scripts/bump-release.py` 不在本卡写入范围）。改进项（默认干跑 + 显式 `--apply`）已并入
  `task-147` 的后续。

## 4. 三条 0 判据 + 开关（提交 1 实测）

| 判据 | 实测 |
|---|---|
| `site/**` 里 pinned 直链 `releases/download/v0.8.36` | **0**（此刻必然 404 ⇒ 不给） |
| 上一版**真实**字节数 `47,431,435` / `42,919,784` | **0**（留着 = 站点带错数字上线） |
| `site/**` 里 `0.8.35` 残留 | **0** |
| `PUBLISHED`（jsonld / geo） | `False` / `False` |
| 「正在发布」/`publishing now` | 4 / 2 |
| 六条站点版本断言 + `Cargo.toml` + 两个 wasm 页（手工核） | 全部 `0.8.36` |
| 部署静态检查本地复刻（必需文件/根绝对路径/CSS/`en/` 前缀） | **fail=0** |

## 5. 门禁

**Lead 实测（tag 目标 `c543515`）**：`BUILD_LOCK_STRICT=1 ./scripts/check.sh --no-release-build`
⇒ **`GATE_EXIT=0`**，窗口 **18:14:00–18:22:47**（构建锁 ~527s）：

```
Test Files  36 passed (36)
     Tests  357 passed | 1 todo (358)        ← **无 Errors 行**
✓ 站点声明的版本与 Cargo.toml 一致：0.8.36
✓ 与 CI 相同的全部检查通过
```

⚠️ **同一提交的第一次运行是 `GATE_EXIT=75`**，原文
`⚠️ 检测到 12 个没有持锁的 cargo/rustc 进程 … ✗ strict ⇒ 明确失败（75）`。
那是**环境问题不是代码失败**（另一个隔离 worktree 在编译；`75` 的定义就是环境问题），
Lead 已开 `task-162` 修这条过宽判据（只对同一 target dir 的未持锁 cargo 失败，跨 target dir 降级为 warning）。

⚠️ **看退出码 + `Errors` 行，不看通过数**：本版绿的计数是 `357 passed`，
而 v0.8.35 期间**同一个数量级**的计数曾经同时带 `Errors 1 error` 被判红 —— 计数相同、结论相反。

## 6. helper 侧「没有新的行为修复」——**我（ops）自己的 diff 自查**（冻结提交 `941f71d`）

| 检查 | 结果 |
|---|---|
| `git diff --stat v0.8.35..HEAD -- crates/xt-proto` | **空** ⇒ 协议号来源未变 ⇒ 设置页不会报「助手版本不匹配」 |
| `controller.rs` 新增 118 / 删除 0 行 | **118/118 全部落在新文件第 375 行之后的 `#[cfg(test)]` 模块内**（逐 hunk 行号判定） |
| `mod.rs` | `run()` 里的注入钩子**整个包在 `#[cfg(test)]`**（生产编译出来就是 `real_run` 直调）；`run_ok`/`args` 行为不变 |
| `snapshot.rs` | `snapshot_dir()` 增加**测试构建专用**临时根，生产分支仍是 `SNAPSHOT_DIR` |
| `server.rs` 生产区 | `uninstall_outcome(` 只有 **2 处**（第 699 行调用 + 第 734 行定义），其余 11 处在测试区 |
| **两条卸载文案逐字** | v0.8.35 与冻结提交**各出现 1 次**（`"helper 已卸载；但回滚网络配置失败：{why} —— 请用「修复网络」再试一次"`、`"helper 已卸载"`；`.into()` → `.to_string()` 语义相同） |
| **优先级语义** | 旧：内存那份先赋值 + `force_cleanup` 用 `get_or_insert_with` 补 ⇒ **内存失败优先**；新：`rollback.err().or(force.err())` ⇒ **同样内存优先**，任何一处失败都不丢 |
| 锁中毒路径 | 旧 `if let Ok(guard)` 失败 ⇒ 不回滚不报失败；新 `Err(_) => Ok(())` ⇒ **一致**（不谎报成功） |
| 两个 crate 全字面量对比 | 450 → 490；**消失 3 条全是测试断言文案/源码片段**，无用户可见文案消失 |

**边界（必读）**：上表是**文本/结构层**的检查，能挡住「用户可见文案被改/被删」这类回归，
但**不是行为证明**（逻辑改了而字符串没变的情况它抓不到）。行为层面的证据是
backend-dev 的用例与 tester 的 `task-157`（独立验证），我不代替它。

## 7. 真实资产（`gh release view v0.8.36`）

```
$ gh release view v0.8.36 --json isDraft,isPrerelease,publishedAt,tagName,assets
{"tagName":"v0.8.36","isDraft":false,"isPrerelease":false,"publishedAt":"2026-09-23T10:46:04Z",
 "assets":[
   {"name":"SHA256SUMS.txt","size":200,"state":"uploaded",
    "digest":"sha256:30508e3741bd7c4191b3d49b0090ed0604a0d1aba4e9edbe8465dcc96dc0c13b"},
   {"name":"XrayTun_0.8.36_x86_64_arm64.dmg","size":47446724,"state":"uploaded",
    "digest":"sha256:70ca6a22e03634f6763c21be3e31a6932fb5a2cbd94ff780c23f2e3a9a5ef8d7"},
   {"name":"XrayTun_0.8.36_x86_64_arm64.zip","size":42936627,"state":"uploaded",
    "digest":"sha256:2f41f076291ad4554784199913d7cc13ec53c90809ed5fa0b65c9a6211ef51fe"}]}
```
**Release workflow**：run `35848485280`（tag `v0.8.36`，`c543515`）⇒ **completed / success**。

**三方一致（zip 是两条路径互证）**：
| 三方 | SHA256SUMS.txt | dmg | zip |
|---|---|---|---|
| GitHub `assets[].digest` | `30508e37…c13b` | `70ca6a22…f8d7` | `2f41f076…51fe` |
| 我本地 `shasum -a 256`（`gh release download` 后） | `30508e37…c13b` ✓ | （未下载，见下） | `2f41f076…51fe` ✓ |
| 发布资产内 `SHA256SUMS.txt` 原文 | — | `70ca6a22…f8d7` ✓ | `2f41f076…51fe` ✓ |

⚠️ dmg 的 47 MB **我没有下载复算**：一致性是「GitHub digest ↔ 发布资产里 `SHA256SUMS.txt` 那一行」
**两方互证**（证据强度与 zip 不同，别混着引）。MiB = **ROUND_HALF_UP 一位小数**：
47,446,724 → **45.2**、42,936,627 → **40.9**。与上一版对照：dmg 47,431,435 → **47,446,724**（+15,289）、
zip 42,919,784 → **42,936,627**（+16,843）。

## 8. 提交 2（真实字节 + `PUBLISHED=True` + pinned + 生成器重跑）

```
$ python3 scripts/bump-release.py phase2 --new 0.8.36 --dmg-bytes 47446724 --zip-bytes 42936627 \
      --sha-bytes 200 --date 2026-09-23
  …26 条规则逐条 ✓…
✓ 全部 26 条校验通过（顺序模拟）；已写 4 个文件：
    scripts/gen-site-jsonld.py / scripts/gen-site-geo.py / site/index.html / site/en/index.html
```
随后手工跑生成器（`gen-site-jsonld.py gen` + `check` 全过、`gen-site-geo.py`）。

| 判据 | 实测（提交 2 工作树） |
|---|---|
| pinned 直链 `releases/download/v0.8.36` | **30**（每页 8 = 6 条 HTML + JSON-LD 2） |
| 「正在发布」/`publishing now` | **0 / 0** |
| 上一版真值 `47,431,435`/`42,919,784` | **0 / 0** |
| 本版真值 `47,446,724`/`42,936,627` | **5 / 5**；`200 字节` 5 处 |
| `PUBLISHED` / `LAST_PUB` | `True` ×2 / `"2026-09-23"` |
| 部署静态检查本地复刻 | **fail=0** |
| 幂等性 | 已发布态上再跑 `phase2` ⇒ **非 0**（设计如此） |

## 9. 线上验收

```
$ VER=0.8.36 PREV=0.8.35 ./scripts/verify-live-site.sh
总结：全部通过（0 条 warning）            LIVE_EXIT=0
[0] 首页 = 43980 bytes，sha256 70ee7176…      ← 与仓库 site/index.html **逐字节相同**（cmp 相同）
[1] 随机不存在路径 → HTTP 404（真 404；未观察 SPA 兜底）
[2] og-image-0.8.36.png 200/image/png/59013 · og-image-en-0.8.36.png 200/image/png/42294（与仓库同字节）
    robots.txt / sitemap.xml / llms.txt / llms-full.txt 均为真文件
[4] www.xraytun.top → 301 https://xraytun.top/
[5] releases/latest → https://github.com/harodggg/xrayTun/releases/tag/v0.8.36 200
[6] GitHub Pages 镜像对不存在路径是真 404

$ ./scripts/verify-live-site.sh --self-test
self-test：四例全部符合预期（异常样例 ✗ / 缓存残留只 WARN / 真 404 正常）     SELFTEST_EXIT=0
```

**提交 2 落地后我自己的 curl 复核（时点 2026-09-23 18:49:58 +0800）**：

```
zh 页 43980 B sha256 70ee7176…  pinned=8 「正在发布」=0  0.8.35 残留=0
     真实字节在页面上：47,446,724 ×1 · 42,936,627 ×1 · 45.2 MiB ×4 · 40.9 MiB ×2
en 页 pinned=8  publishing=0  0.8.35=0  「47,446,724 bytes」×1
cmp：zh 线上 == 仓库 ✓ ；en 线上 == 仓库 ✓
og 图：0.8.36 与 -en 均 200 且与仓库同字节（59,013 / 42,294）—— v0.8.35 那次 404 窗口**没有回来**
```
部署：`Pages` 与 `Cloudflare Pages` 两个 workflow 对 `03cc02f` 都是 **completed / success**。

## 10. `.app` 三脚本（**发布资产 zip**）

```
$ ditto -x -k XrayTun_0.8.36_x86_64_arm64.zip /tmp/v0836-app
$ docs/verification/verify-app-bundle-resources.sh /tmp/v0836-app/XrayTun.app
  ✓ tauri.conf.json 声明的 scripts/* 恰好是 3 个：incident-bundle.sh net-metrics.py triage-incident.py
  ✓ scripts/incident-bundle.sh == scripts/incident-bundle.sh（sha256 7662c2193a9e6bf7…）
  ✓ scripts/triage-incident.py == scripts/triage-incident.py（sha256 6969e38b5355bab4…）
  ✓ scripts/net-metrics.py == scripts/net-metrics.py（sha256 e45318da088cb16f…）
  实际 Contents/Resources/scripts/：
    -rwxr-xr-x@ 1 xbtg- staff 36701 9月 23 18:23 incident-bundle.sh
    -rw-r--r--@ 1 xbtg- staff 49932 9月 23 18:23 net-metrics.py
    -rw-r--r--@ 1 xbtg- staff 45288 9月 23 18:23 triage-incident.py
  ✓ codesign --verify --strict 通过
pass=5 fail=0
```
（校验器自带 `--self-test` 4/0 与「路径写错 ⇒ 报红」的反向断言，见 `TASK-147` 之外的
`verify-app-bundle-resources.sh` 说明。）

## 11. Release Notes 机制的真实环境验证（`task-147`）

**结论：机制在真 CI 上生效（逐字节）** —— 这是 `task-147` B 部分的真实环境验证：

```
$ python3 scripts/bump-release.py notes --tag v0.8.36 --out /tmp/v0836-notes-local.md
[notes] 动作来源 docs/release-notes/v0.8.36.md（3136 字节，有必须的用户动作）
[notes] 写出 /tmp/v0836-notes-local.md（6032 字节）

$ gh release view v0.8.36 --json body --jq '.body' > /tmp/v0836-body-ci.md     # 6033 字节

$ cmp <(cat /tmp/v0836-notes-local.md; printf '\n') /tmp/v0836-body-ci.md
✓ **逐字节相同**（本地 NOTES.md + "\n" == CI 读回 body）
关键行复核：`本版没有新的助手侧行为修复` ×1 、`^## 安装` ×1（固定模板在后）
```
⇒ ①`release.yml` 的新接线**在真实 CI 上跑通了**（不是只在本地假 `gh` 的复刻里）；
②「版本特有动作」确实来自 `docs/release-notes/v0.8.36.md`（它是提交 1 的一部分，在 tag 树里）。
**本版不再需要**发布后手工 `gh release edit` —— 这正是 `task-147` 要消灭的那一步。

## 12. 本版对照基线（`net-metrics.py`，带口径）

```
$ python3 scripts/net-metrics.py --until "2026-09-23 18:23:02"     # = tagger epoch 1790158982
口径头（引用请连这一段一起引）：
  N = 窗口内命中 705,563 条（移动标记：日志在被持续追加）
  T = 窗口右端 2026-09-23 18:23:02（移动标记）
  日志：app.1.jsonl = 92,344,495 B / mtime 16:15:03；app.jsonl = 48,567,865 B / mtime 18:23:42
  切：全部 → T（本地时区）
  解：全文件扫描 706,842 行 → 706,842 个对象；多对象行 0；坏行 0；message 含 LF = 0
  匹配：键 (ts_unix, message)、先见者胜、跨文件 app.1 → app；丢弃重复 16 条（窗口内 16）
  来源：source=app 11,943 / source=core 693,620 / 其它 0
  选择内容指纹：sha256:43d98fad633a5629c220d5bb87a36cf92ef10e38008e5cd29aacef9a64b03712
```

| 指标 | 值（同一窗口） |
|---|---|
| `replace destination with tcp:[240e…` | **0**（v0.8.34 修的 v6 改写，仍为 0） |
| `failed to open connection` | 13 |
| 探针连接 / 成功 / 失败·有 failed 行 | 1,563 / 1,557 / 6（探针轮 1,552） |
| `cp.cloudflare.com:80` 成功耗时 | 中位 644.8ms、p90 1073.1、p95 1778.0、p99 4703.9、max 857065.7（n=1,557） |
| `>6s 才成功` | **11 条**（v0.8.34 已把探针超时放宽到 10s 的依据） |
| 「已作废」/「隧道已自动恢复」 | **3 / 1**（口径A/B：21:03:33→2262s、22:14:42→44859s、16:17:46→50s） |
| `core 启动` | 10 次 |
| 「物理出口已变化」 | 0 |

完整输出 48 行：`/tmp/v0836-netmetrics.txt`。

## 13. Release Notes 的版本特有动作（机制）

`docs/release-notes/v0.8.36.md` 在**打 tag 之前**进了 main（提交 1）—— 这是 `task-147`
机制的要求（`release.yml` 从它取「本版用户动作」，缺了发布步骤会**直接失败**）。
本版是这套机制的**第一次真实使用**：§11 的逐字节对照就是它的真实环境验证。

## 14. 诚实清单

* **真机安装 / 重启 / Gatekeeper / 重装助手未在本环境验证**；「本版不需要重装助手」的依据是
  §6 的源码 diff + 依赖关系 + 字面量对比，**不是**在这台机器上真的重装过一次；
* 用户装上新版之后的「after」数字**现在不存在**，本文件不预填、不推测（§12 全是发版前的基线；
  §12 的数字还在随时点增长 —— **移动标记**）；
* §6 的字面量对比是**代理指标**，不是行为证明（见 §6 的边界）；我第一版用
  「生产区 = 第一个顶层 `#[cfg(test)]` 之前」做区域对比，在**被重组的文件**（`snapshot.rs`）上
  **假红**（区域边界随文件重排移动）—— 那是我方法的错，已换成整文件字面量集合对比；
* 门禁那两次（75 → 0）说明 strict 判据当前**过宽**（`task-162`）；本文件只报事实，不改判据；
* `scripts/check.sh` 的 6 条站点版本断言仍**不覆盖** `Cargo.lock` 与 `site/{,en/}wasm/index.html`
  （后者本版是**手工核**的）；
* 站点两阶段发布：提交 1 阶段官网如实写「正在发布」（已实测），提交 2 之后为已发布态；
* `bump-release.py` 的开关是 `--dry-run`（**默认落盘**）—— 卡面写的 `--apply` 不存在；
  改进项（默认干跑 + 显式 `--apply`）已并入 `task-147` 的后续，本版**没有**在冻结窗口改工具；
* **Release 正文与本地合成逐字节相同**这条只证明「CI 用了这份单一来源」，
  不证明「用户真的看到了它」—— 我没法验证任何客户端/平台缓存；
* 部署时序：`Pages` / `Cloudflare Pages` 对 `03cc02f` 都 success 之后我才做线上复核（18:49:58）；
  若之后 CF 缓存行为变化，本节数字属于**该时点**的快照。
