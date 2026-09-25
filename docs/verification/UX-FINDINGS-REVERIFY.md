# 复核：UX 5 条高危是否被 frontend 改动消除（task-10 / tester 独立验证）

**复核对象**：`docs/verification/UX-REVIEW-INTENT-MITM.md`（UX，task-5，提交 `c0be892`，按 `0e09214` 记行号）提出的 5 条高危。
**复核基准**：**新提交 `e2b4d06`**（`git rev-parse HEAD`），`apps/ui` 无未提交改动（`git -C xray-tun status --porcelain -- apps/ui/src` 输出 0 行）。
**被验改动**：frontend 提交 `7b28ba0`（`fix(ui): 界面不许撒谎也不许吓人`）+ 后续相关 UI 改动。

## 结论 + 证据

| 条目 | 判定 | 一句话依据 | 证据 |
|------|------|-----------|------|
| **U1** MITM「生效」把「核心没在跑」说成「已随核心生效」 | ✅ **已消除** | `core_steering === null` 现在是独立一态，渲染与总结句都单列 | `Intent.tsx:206-208`（总结）、`Intent.tsx:748-749`（渲染）；测试 `failureHonesty.test.tsx:449-460` |
| **U2** 审计列「生效 = 是」被读成「正在被拦」 | ✅ **已消除** | 列名改「会生成规则」，单元格改「会 / 不会（演练模式）/ 不会（不是拦截判决）」，页面注释改成正向口径 | `Intent.tsx:933`、`Intent.tsx:971-977`、`Intent.tsx:905-922`；测试 `:486-497` |
| **U3** 失败横幅无动作 / 无「重装助手」入口 / 文案带 `**` 且换行坍缩 | ⚠️ **部分消除** | 横幅与仪表盘 notice 已修（动作 + `stripMarkup` + `pre-wrap` + 测试）；**但顶栏 `status.detail`/`sub` 仍原样透传 `lastError`**，`**` 与 `\n` 会进入 hover tooltip 与读屏 live region | 已修：`App.tsx:193-223`、`failure.ts:203-238`、`styles.css:626-628`、`Dashboard.tsx:505-542`；**残留**：`topbarStatus.ts:228-236` → `App.tsx:335,338,418`（实跑证明见 §U3） |
| **U4** 启动即崩时 `logs/panic.log` 在 App 内 0 命中 | ⚠️ **部分消除** | App 内确实从 0 命中变成常驻一行 + 「打开数据目录」动作，并有渲染断言；但**「App 起不来」这个真实场景的载体没有新增**（`site/` 无排障页、`package-macos.sh` 无 panic 说明），仍只有既有的发布说明 | 已修：`Logs.tsx:45-57,243-264`；测试 `failureHonesty.test.tsx:572-590`；`grep -rn panic apps/ui/src | wc -l` = 13（原 0）。缺口：`ls site/` 无排障页、`grep -n panic scripts/package-macos.sh` 无输出 |
| **U5**（UX 报告里的 **U8**）横幅叫用户点「断开」而按钮写「连接」 | ⚠️ **部分消除** | 新增 `buttonNameNote()`，在横幅与 notice 同屏说明「按钮现在写的是『连接』」，有测试；**但后端那句误导文案本身没改**（`supervisor.rs:452,457` 原样保留），用户仍会先看到「先点『断开』恢复直连」再看到更正 | 新增：`failure.ts:231-238`；接线：`App.tsx:112,212`、`Dashboard.tsx:512,525`；测试 `:293-303`、`:545-570`。未改：`supervisor.rs:452,457` |

**我实际跑了什么**（全部在 `e2b4d06`）：
- `apps/ui/node_modules/.bin/vitest run` → **42 files / 447 passed / 1 todo，exit 0**（`/tmp/t10-vitest-full.log`）
- `vitest run src/failureHonesty.test.tsx src/intentPage.test.tsx` → **41 passed，exit 0**（`/tmp/t10-vitest-targeted.log`）
- 逐条 `grep`/`sed` 重算行号 + **实跑 `appStatus()`**（esbuild 打包后 node 执行）证明残留（§U3）

---

## 复核基准：行号怎么算的

```bash
cd /Users/xbtg-/deepseek-harness/xray-tun
git rev-parse HEAD                      # e2b4d06b4d380ecdba73357fe2c7b993248d0504
git status --porcelain -- apps/ui/src   # （空 ⇒ 下面行号就是该提交的内容）
```

UX 报告里的旧行号（`Intent.tsx:453-460`、`:620`、`:646`、`App.tsx:159-166` …）**全部作废**；下面每条都按 `e2b4d06` 重算。

---

## U1 ｜ 已消除

**旧问题**：`core_steering === null`（核心从没启动/已停）时后端给 `core_restart_required=false`，旧代码落到 `mitm.active`（只等于「开关开 + 名单非空」）⇒ 写「引导规则已随核心生效」。

**新代码**（`e2b4d06`）：

```
apps/ui/src/pages/Intent.tsx:206-208   // 总结句：null 单独一态
  if (status.core_steering === null) {
    return "三道闸门全过 —— 但核心没在跑，引导规则现在不在任何核心里：此刻没有域名被拆 TLS（先连接核心）。";
  }
apps/ui/src/pages/Intent.tsx:748-749   // 状态表：null 单独一态（顺序在 core_restart_required 之前）
  : mitm.core_steering === null
    ? "核心没在跑 —— 引导规则现在不在任何核心里（先连接核心）"
```

`"引导规则已随核心生效"` 仍然存在，但只剩 `Intent.tsx:753` 一处，且被 `:752 mitm.core_steering ?` 严格保护（只有后端明确给 `true` 才说）。

**测试（实际渲染断言）**：`failureHonesty.test.tsx:449-460`

```
it("U1：装了证书 + 起了代理但**核心从没启动** ⇒ 不许写「引导规则已随核心生效」")
  mocks.mitmStatus.mockResolvedValue(mitmStatus({ core_steering: null, core_restart_required: false, note: null }))
  expect(text).toContain("核心没在跑 —— 引导规则现在不在任何核心里（先连接核心）")
  expect(text).not.toContain("引导规则已随核心生效")      ← 负断言
  expect(text).toContain("此刻没有域名被拆 TLS")
```

实跑结果：`vitest run src/failureHonesty.test.tsx` → `29 passed`（含此条）。

**判定**：✅ 已消除。旧代码路径（null → 说已生效）已不存在。

---

## U2 ｜ 已消除

**旧问题**：表头 `<th>生效</th>`、单元格 `是`，而 `applied` 的语义只是「判定那一刻构成规则」（`crates/xt-intent/src/engine.rs:444-446`），与「有没有下发给正在跑的核心」无关。

**新代码**：

```
apps/ui/src/pages/Intent.tsx:933      <th>会生成规则</th>
apps/ui/src/pages/Intent.tsx:971-977  {row.applied ? "会"
                                        : row.outcome === "block" ? "不会（演练模式）"
                                        : "不会（不是拦截判决）"}
apps/ui/src/pages/Intent.tsx:905-922  页面注释改成：「『会』不代表规则已经下发到核心 …」
```

`grep -rn "<th>生效</th>" apps/ui/src` → **无输出**（旧表头已消失）。

**测试**：`failureHonesty.test.tsx:486-497`（渲染断言）

```
expect(screen.getByText("会生成规则")).toBeTruthy();
expect(within(row).getByText("不会（演练模式）")).toBeTruthy();
expect(within(row).getByText("本该拦截（演练）")).toBeTruthy();
expect(within(row).queryByText("拦截")).toBeNull();          ← 绿色「拦截」徽章不再出现
expect(document.body.textContent).toContain("不代表规则已经下发到核心");
```

**判定**：✅ 已消除（列名 + 单元格 + 注释三处都改；且注明了它不等于已下发）。

---

## U3 ｜ ⚠️ 部分消除（横幅与 notice 已修，顶栏仍透传原始文案）

### 已消除的部分

1. **动作**：`App.tsx` 的全局横幅现在渲染可点按钮：

```
apps/ui/src/App.tsx:111   const errorActions = error ? failureActions(error) : [];
apps/ui/src/App.tsx:213-222  <div className="banner__actions">{errorActions.map(a => <button …>{a.label}</button>)}</div>
```

`failure.ts:203-211` 给出动作：`去重装助手` / `去换一个节点` / `查看日志`（助手类文案 ⇒ 重装助手；节点/连接类 ⇒ 换节点）。
仪表盘 rank 0 的 `last-error` notice 也从「无 action」变成有 action：

```
apps/ui/src/pages/Dashboard.tsx:513  const primary = failureActions(raw)[0] ?? null;
apps/ui/src/pages/Dashboard.tsx:535-542  action: { label: primary.label, run: … onNavigate("settings","set-helper") / ("nodes") / ("logs") }
```

2. **`**` 与换行（横幅/notice）**：

```
apps/ui/src/App.tsx:198   <div className="banner__reason">{stripMarkup(error)}</div>
apps/ui/src/styles.css:626-628  .banner__reason { white-space: pre-wrap; word-break: break-word; }
apps/ui/src/failure.ts:218-220  stripMarkup = (t) => t.replace(/\*\*(.+?)\*\*/g, "$1")
```

3. 「重装助手」入口：`failureHonesty.test.tsx:342-363`（助手类 ⇒ 有「去重装助手」且不带 `**`、保留换行）、`:364-386`（门禁类 ⇒ 「去换一个节点」真的导航到节点页）、`:545-570`（Dashboard 门禁类：原文去 `**` + 补按钮名）。

**实跑**：`vitest run src/failureHonesty.test.tsx` → 29 passed（以上均在）。

### 未消除的部分（差在哪一步）

**差在顶栏这一路没走 `stripMarkup`**：`lastError` 从 `Dashboard.tsx:216` 传进 `appStatus()`，`topbarStatus.ts:228-236` 把它**原样**拼进 `sub`/`detail`：

```
apps/ui/src/topbarStatus.ts:228  if (lastError !== null && !running) {
apps/ui/src/topbarStatus.ts:234    sub: advice ? `${lastError} —— ${advice}` : lastError,
apps/ui/src/topbarStatus.ts:235    detail: advice ? `核心未运行 —— ${lastError}；${advice}` : …,
```

而 `detail` 会渲染到：`App.tsx:335`（`title` hover tooltip）、`App.tsx:338`（`role="status"` 读屏 live region）、`App.tsx:418`（badge 的 `title`）。

**实跑证明**（把真实的 `topbarStatus.ts` 用仓库自带 esbuild 打包后执行，不是读代码猜的）：

```bash
cd apps/ui
./node_modules/.bin/esbuild src/topbarStatus.ts --bundle --format=esm --outfile=/tmp/t10-topbar.mjs
node --input-type=module -e '
import { appStatus } from "/tmp/t10-topbar.mjs";
const raw = "节点通过了 TCP 检查……**已在接管默认路由之前中止**，系统网络未被改动。\n**这不是本机 DNS 的问题**。先点「断开」恢复直连。";
const r = appStatus({ mode:"tun", running:false, routesCommitted:false, lastError:raw,
  corePath:"/x", recovery:{phase:"idle"}, snapshotLoaded:true, socksPort:10808, httpPort:10809 });
console.log(r.detail.includes("**"), r.sub.includes("**"));'
```

输出：

```
label    = 核心未运行
sub      = "节点通过了 TCP 检查……**已在接管默认路由之前中止**，系统网络未被改动。\n**这不是本机 DNS 的问题**。先点「断开」恢复直连。 —— 下一步：换一个节点再试…；看日志：…"
detail   = "核心未运行 —— 节点通过了 TCP 检查……**已在接管默认路由之前中止**…；下一步：…"
detail 含 ** ? true | sub 含 ** ? true
```

`grep -rn "stripMarkup" apps/ui/src` 只命中 `failure.ts`（定义）、`App.tsx:198`、`Dashboard.tsx:522` —— **`topbarStatus.ts` 没有**。

**其他残留风险（不是「未消除」，但要记一笔）**：动作是按**后端文案的文本线索**匹配的（`failure.ts:144-152` 的正则），不是结构化 `failure_kind`。后端 `supervisor.rs` 文案一改字，`去重装助手` 就可能静默消失 —— 没有测试锁住这条耦合。

**判定**：⚠️ 部分消除。横幅/notice 三条症状都修了；顶栏 `sub`/`detail`（tooltip + 读屏）仍会把 `**` 与 `\n` 原样交给用户/读屏，且**现有 42 个测试文件没有一条覆盖它**。

---

## U4 ｜ ⚠️ 部分消除（App 内已加，App 外的载体没加）

**旧问题**：`grep -rn "panic" apps/ui/src | wc -l` = 0 —— App 内没有任何地方提 `logs/panic.log`。

**已修（App 内）**：

```
apps/ui/src/pages/Logs.tsx:45-57    export const PANIC_LOG_PATH = "~/Library/Application Support/com.xraytun.desktop/logs/panic.log";
apps/ui/src/pages/Logs.tsx:243-264  常驻一行：「如果 App 曾经启动就退出：崩溃证据在数据目录的 logs/panic.log（默认 …，内含 文件:行 与 backtrace）」+「打开数据目录」按钮（复用 api.openDataDir()）
```

现状统计：`grep -rn "panic" apps/ui/src | wc -l` → **13**（原 0）。

**测试**：`failureHonesty.test.tsx:572-590`

```
await screen.findByRole("button", { name: "打开数据目录" });
expect(text).toContain("panic.log");
expect(text).toContain(PANIC_LOG_PATH);
expect(text).toContain("文件:行");
fireEvent.click(…); await vi.waitFor(() => expect(mocks.openDataDir).toHaveBeenCalledTimes(1));
```

**未消除的部分（差在哪一步）**：U4 的**要害**是「App 启动即崩 ⇒ 界面根本打不开」（这一点我在 task-9 里用本机构建的 0.8.39 App 复现过：`open -a` 3/3 panic）。这条路上唯一有帮助的载体必须在 App 之外，而本次改动的范围是 `apps/ui/src/**`，所以：

```
ls site/                      → 无「启动崩溃怎么办」类排障页（只有产品页/项目页/wasm）
grep -n "panic" scripts/package-macos.sh  → 无输出（DMG 里没有这句说明）
grep -rln "panic.log" docs/release-notes  → docs/release-notes/v0.8.39.md:21,26（既有，未新增）
```

即：**App 内**的可发现性已从 0 命中修好（且有断言）；**App 起不来时**的载体（站点排障页 / DMG 说明 / 发行物内可达的发布说明）没有新增，仍只有一份用户不一定找得到的发布说明。

**判定**：⚠️ 部分消除。要完全消除 U4，需要一条「不依赖 App 能启动」的载体（站点页或 DMG 说明），不属 `apps/ui/src` 范围。

---

## U5 ｜ ⚠️ 部分消除（前端加了更正说明，后端误导文案没改）

**旧问题**：门禁失败文案 `先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。`（`supervisor.rs:452,457`），而这一刻 `running=false`、顶栏按钮写「连接」。

**已修（前端补偿）**：

```
apps/ui/src/failure.ts:231-238   buttonNameNote(text, running)：running=false 且文案含「断开」时，
                                 返回「…顶栏右上角那个按钮现在写的是「连接」，不是「断开」…」
apps/ui/src/App.tsx:112          const errorNameNote = error ? buttonNameNote(error, running) : null;
apps/ui/src/App.tsx:212          {errorNameNote && <div className="banner__steps">{errorNameNote}</div>}
apps/ui/src/pages/Dashboard.tsx:512,525   notice 里同样加 note
```

**测试**：`failureHonesty.test.tsx:293-303`（纯函数：在跑时不啰嗦、没在跑且提「断开」时说清按钮名）、`:545-570`（Dashboard 门禁类 notice 文本含「写的是「连接」」，且动作是「去换一个节点」）。

**未消除的部分（差在哪一步）**：后端文案**一个字没改**，仍原样渲染：

```
apps/desktop/src/supervisor.rs:452  "先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。"
apps/desktop/src/supervisor.rs:457  （同一句的另一分支）
```

`git diff 0e09214..e2b4d06 -- apps/desktop/src/supervisor.rs | grep -n "断开"` → 无输出（这条文件在这段区间里没有针对该句的改动）。
所以用户在同一屏里会先读到「先点『断开』」，再读到「…按钮现在写的是『连接』，不是『断开』」——**更正有了，误导的原文还在**。UX 报告建议的「按 `running` 拆分文案」或改成不依赖按钮名的说法，**没有落**。

**判定**：⚠️ 部分消除。用户不再「照着找不到按钮」而无人解释，但那句错话仍由后端产出、前端只做补丁；这属于 task-3 范围外的后端文案（`apps/desktop/src/**`）。

---

## 我跑了什么 / 我没跑什么

### 跑了（都在 `e2b4d06`）

| 命令 | 结果 | 日志 |
|------|------|------|
| `cd apps/ui && ./node_modules/.bin/vitest run` | **42 files / 447 passed / 1 todo / exit 0** | `/tmp/t10-vitest-full.log` |
| `./node_modules/.bin/vitest run src/failureHonesty.test.tsx src/intentPage.test.tsx` | **2 files / 41 passed / exit 0** | `/tmp/t10-vitest-targeted.log` |
| `./node_modules/.bin/vitest run src/failureHonesty.test.tsx --reporter=verbose` | 29 条用例名逐条可见（U1/U2/U5/U8/U4/横幅/notice 都在） | `/tmp/t10-vitest-verbose.log` |
| `esbuild src/topbarStatus.ts` + `node` 实跑 `appStatus()` | 证明 `running=false` 时 `sub`/`detail` **含 `**` 与 `\n`** | 见 §U3 |
| 逐条 `grep -n` / `sed -n` 重算行号（U1/U2/U3/U4/U5） | 见各条 | 本报告内命令 |

### 没跑（明确写清楚）

1. **没有真机 GUI 走查**：没有在浏览器/真实窗口里看 hover tooltip、读屏 live region 的实际朗读、`pre-wrap` 的像素换行效果。U3 的残留是**执行真实纯函数 + 渲染点定位**证明的，不是像素级比对。
2. **没有跑后端 / 没有构造真实失败**：`core_steering=null`、门禁失败、旧 helper 协议不匹配都是**前端 mock 的载荷**；没有跑真实核心/helper 去产生这些状态。
3. **没有验证「重装助手」动作在真实场景可达**：动作由文案正则推出，我只验证了「文案含 helper/助手 ⇒ 有该动作」（测试），**没有**验证 v0.8.38 那种 TUN 协议不匹配错误最终真的以含 `helper` 的字符串进 `runtime.last_error`。
4. **没有验收 `site/`、`package-macos.sh`、发布说明这些 U4 的 App 外载体**（它们不在 task-3 范围内；我只做了 `ls`/`grep` 确认「没有新增」）。
5. **没有跑 `scripts/check.sh`**（本任务不要求；该门禁的全量结果见 `docs/verification/TEST-REPORT-0.8.39.md`）。
6. **没有改任何产品代码**：只新建本文件；`apps/ui` 在复核时 `git status --porcelain -- apps/ui/src` 为 0 行。

---

## 附录 A：可复用的「启动崩溃」复现/验证脚本（task-14 用）

我在 task-9 里用的原始脚本在 `/tmp/tester-crash-repro.sh`。**这里给一版针对 task-14（验证修复）加固过的**，两处关键教训必须保留：

1. **`open -n --env XRAYTUN_DATA_DIR=… -a <app>` 的环境变量不会传进去** —— 我实测 panic.log 落在**真实**数据目录、隔离目录为空。所以 `open` 阶段**不受**隔离保护。
2. 因此在修复后谨慎使用：**修复前** panic 发生在 `setup`，还没来得及自动重连；**修复后 App 会真的启动**，如果真实 `settings.json` 里 `was_connected=true` 且 `mode != direct`，它就会自动重连并接管默认路由。脚本先做**前置检查**，不安全就拒绝跑 `open` 阶段。

```bash
#!/bin/bash
# task-14 用：验证「启动即崩」是否修好。
#   · 直接 exec：用 XRAYTUN_DATA_DIR + mode=direct + was_connected=false 三重保护（沙箱可写）
#   · open -a   ：**环境变量不生效**，会用真实 settings ⇒ 先检查真实 settings，不安全就直接拒绝
# 用法：bash verify-startup-crash.sh /path/to/XrayTun.app
set -u
APPDIR="${1:?usage: $0 /path/to/XrayTun.app}"
BIN="$APPDIR/Contents/MacOS/xraytun-desktop"
ISO=/Users/xbtg-/deepseek-harness/.tester-appdata
OUT=/tmp/tester-crash; REPORTS="$HOME/Library/Logs/DiagnosticReports"
REALDATA="$HOME/Library/Application Support/com.xraytun.desktop"
REAL_PANIC="$REALDATA/logs/panic.log"
mkdir -p "$OUT" "$ISO/logs"

route() { netstat -rn -f inet | awk '$1=="default"{print $2" "$NF}' | head -1; }
BASELINE="$(route)"; echo "BASELINE_ROUTE=${BASELINE:-<none>}"

# 隔离数据目录：mode=direct 且 was_connected=false ⇒ 不可能自动重连（should_auto_reconnect 要求三条件）
python3 - "$REALDATA/settings.json" "$ISO/settings.json" <<'PY'
import json,sys
s=json.load(open(sys.argv[1])); s["mode"]="direct"; s["was_connected"]=False
s["auto_reconnect"]=False; s["selected_node"]=None
json.dump(s,open(sys.argv[2],"w"),ensure_ascii=False,indent=2)
PY

# ---- open 阶段的安全前置检查（因为 --env 不生效）----
open_safe=1
if [ -f "$REALDATA/settings.json" ]; then
  read -r mode was <<<"$(python3 -c "
import json;s=json.load(open('$REALDATA/settings.json'))
print(s.get('mode','?'), s.get('was_connected'))")"
  if [ "$was" = "True" ] && [ "$mode" != "direct" ]; then
    echo "!! 拒绝跑 open 阶段：真实 settings 是 mode=$mode / was_connected=$was"
    echo "   修复后 App 会真的启动并自动重连（TUN 会接管默认路由）。"
    echo "   请先把真实 settings 的 was_connected 置 false（备份后），或让有写权限的人来做。"
    open_safe=0
  fi
fi

real_before=$(stat -f%z "$REAL_PANIC" 2>/dev/null || echo 0)
ips_before=$(ls -1 "$REPORTS" 2>/dev/null | grep -c xraytun)

run_direct() {
  local tag="direct-$1" pid cur; : >"$OUT/$tag.out"
  rm -f "$ISO/logs/panic.log"
  RUST_BACKTRACE=1 XRAYTUN_DATA_DIR="$ISO" XRAYTUN_LOG=debug "$BIN" >>"$OUT/$tag.out" 2>&1 &
  pid=$!
  for i in $(seq 1 40); do
    kill -0 "$pid" 2>/dev/null || break
    cur="$(route)"; [ "$cur" != "$BASELINE" ] && { echo "[$tag] ROUTE_CHANGED -> $cur"; pkill -9 -f "$BIN"; break; }
    sleep 0.5
  done
  kill -0 "$pid" 2>/dev/null && { kill -TERM "$pid"; sleep 2; kill -9 "$pid" 2>/dev/null; }
  wait "$pid" 2>/dev/null; local code=$?
  echo "[$tag] exit=$code  iso_panic_bytes=$(stat -f%z "$ISO/logs/panic.log" 2>/dev/null || echo 0)"
  head -3 "$ISO/logs/panic.log" 2>/dev/null
}

run_open() {
  local tag="open-$1" changed=0 cur
  pkill -f "$BIN" 2>/dev/null; sleep 1
  open -n -a "$APPDIR" >/dev/null 2>&1
  for i in $(seq 1 40); do
    pgrep -f "$BIN" >/dev/null 2>&1 || break
    cur="$(route)"; [ "$cur" != "$BASELINE" ] && { echo "[$tag] ROUTE_CHANGED -> $cur"; pkill -9 -f "$BIN"; changed=1; break; }
    sleep 0.5
  done
  pgrep -f "$BIN" >/dev/null 2>&1 && { pkill -f "$BIN"; sleep 2; pkill -9 -f "$BIN" 2>/dev/null; }
  echo "[$tag] route_changed=$changed"
}

for n in 1 2 3; do run_direct "$n"; done
if [ "$open_safe" = 1 ]; then for n in 1 2 3; do run_open "$n"; done
else echo "（open 阶段已跳过 —— 见上面的安全前置检查）"; fi

real_after=$(stat -f%z "$REAL_PANIC" 2>/dev/null || echo 0)
ips_after=$(ls -1 "$REPORTS" 2>/dev/null | grep -c xraytun)
echo "FINAL_ROUTE=$(route)  REAL_PANIC_BYTES ${real_before} -> ${real_after}"
echo "NEW_IPS=$((ips_after-ips_before))  （0 且 route 不变 ⇒ 启动崩溃已消除）"
```

**判据（task-14 可直接用）**：
- 修复后：`NEW_IPS=0` **且** `REAL_PANIC_BYTES` 不增长 **且** `FINAL_ROUTE` 与基线相同；
- 若 `new_ips>0`，用 `python3` 解 `.ips` 的 `exception.signal` / `termination.indicator` 判断是不是 SIGABRT（本次 0.8.38 现场：`EXC_CRASH` / `SIGABRT` / `abort() called`）；
- **反例（能在旧代码上变红）**：拿修复前的构建跑，`open -a` 应给出 `NEW_IPS=3` 且 panic.log 增长 3 条 —— 这就是 task-14 要求的「门禁守卫必须在旧代码上变红」的现成夹具。

---

复核人：`tester`（不写产品代码）。本文件只新建这一个文件；未 `git add -A`、未 push。
