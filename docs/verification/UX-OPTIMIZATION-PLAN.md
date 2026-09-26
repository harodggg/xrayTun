# UX 优化清单（task-22）—— 按用户旅程排序

**审查员**：ux（task-22）
**复核基准**：本文所有行号取自 **`7ddda64`** 的 `apps/ui/**`；写完后 HEAD 已被队友推进到 `bde1c5d`，但 §0.3 表里这些文件在两者之间 **byte-identical**（已用 `git rev-parse 7ddda64:<f>` vs `git rev-parse HEAD:<f>` 逐个比对，12/12 same）⇒ 行号对当前树同样有效
**范围**：只新建本文，**未改任何 `apps/**` 代码**（实现归 task-23）；**未 push**

---

## 0. 结论、环境事实更正、复核基准

### 0.1 一句话结论

前几轮修掉的都是「说的与事实不符」（假陈述）。这一轮**剩下的大头是三类**，而且都不是文案花活：

1. **空态冒充错误态**：`snapshot` 读不到时，三个页面写「正在加载…」永远不停（`Dashboard.tsx:189`、`Routing.tsx:217`、`Settings.tsx:295`），另外两个直接把空数据渲染成「还没有任何节点 / 还没有订阅」（`Nodes.tsx:152`、`Subscriptions.tsx:82`）—— 用户被告知的是一个**错误的原因**（"你没有节点" vs "没读到节点"）。
2. **无声失败**：忙碌时点「连接」/模式按钮**静默无事发生**（`App.tsx:306-319` + `store.tsx:259-274`）；两处「复制」失败只 `console.log`，在 WKWebView 里等于没说（`Logs.tsx:119-124`、`Nodes.tsx:225`）；一批长操作（更新订阅、检查更新）**没有任何进度**。
3. **可访问性**：全仓 `aria-live` **0 命中**，**最关键的失败横幅**（`App.tsx:189`）偏偏没有 `role="alert"`（而同仓的日志页/规则页有）；节点行的选中**只能鼠标点**（`Nodes.tsx:284` 是 `<div onClick>`）；全仓只有一处 `:focus-visible` 样式（`styles.css:1329`）；`--text-faint` 在 11px 上只有 **3.87:1 / 3.56:1**，低于 WCAG AA 的 4.5:1。

### 0.2 环境事实更正（必须说清）

任务卡写明「本机没有 Xcode CLT ⇒ git/clang 不可用，`cargo`/`vitest` 跑不起来、也提交不了」。**我在本会话实测，这些工具都在且可用**：

```
$ git --version                 → git version 2.50.1 (Apple Git-155)
$ xcode-select -p               → /Library/Developer/CommandLineTools
$ clang --version               → Homebrew clang version 23.1.0
$ cargo --version               → cargo 1.98.0 (797e8a9bc 2026-08-05)
$ node --version                → v26.9.0
$ ls apps/ui/node_modules/.bin/vitest → 存在
```

我据此做了两件任务卡语境下"做不到"的事，**据实报告**：

1. `cd apps/ui && ./node_modules/.bin/vitest run` → **43 files passed / 465 passed + 1 todo**（这就是下面的「现状基线」，也是本文唯一跑过的命令）；
2. **本文按 `7ddda64` 的真实行号引用**，并附 blob 表（§0.3），不靠"工具链坏了所以行号可能不准"免责。

⚠️ **但这不等于任何东西"已验证"**：清单里的改法**一条都还没有实现**（实现是 task-23）。我跑的是**改动前**的基线套件；下面所有「验收判据」都是**待实现后要跑的断言**，不是跑过的结果。

### 0.3 行号复算入口（blob）

```sh
git show 7ddda64:apps/ui/src/App.tsx | sed -n '188,229p'   # D1/D2 的两个失败横幅
git show 7ddda64:apps/ui/src/pages/Nodes.tsx | sed -n '283,307p'  # B1 不可聚焦的行
git show 7ddda64:apps/ui/src/pages/Intent.tsx | sed -n '416,429p' # D3
```

| 文件 | blob @ `7ddda64` |
|---|---|
| `apps/ui/src/App.tsx` | `f4b181d687ffe412ca3f37bfc85002796c5d41fe` |
| `apps/ui/src/store.tsx` | `ccb218755bdce313a642f8b167c01b6cbb95121b` |
| `apps/ui/src/topbarStatus.ts` | `8c7a5357eb9ffc46923ddb703f9c81f5ec088056` |
| `apps/ui/src/pages/Dashboard.tsx` | `46f9ecf2e020994374f3389d294c636217b7ff79` |
| `apps/ui/src/pages/Intent.tsx` | `d9d3c1463b40aea99a34d8b233ecc2ccfd7e282c` |
| `apps/ui/src/pages/Nodes.tsx` | `9c3f4e10e5b63fc8d5dbdb503446b8544ef2b901` |
| `apps/ui/src/pages/Routing.tsx` | `f392ee35b9401c0976bf2581e2e8cd0e0cbcf03f` |
| `apps/ui/src/pages/Logs.tsx` | `9c1237ae1235038cf464a18124fe2671021acac5` |
| `apps/ui/src/pages/Subscriptions.tsx` | `3ee2c5587cc67f69e2b4e5d1be9fb199bc0ed810` |
| `apps/ui/src/pages/Settings.tsx` | `a594b0012bcb16fcf12b0a6e28eea0656370a8f9` |
| `apps/ui/src/styles.css` | `9bc1a7b0b158c53acfb3558514dd2c18fde2c811` |
| `apps/ui/src/InlineConfirm.tsx` | `fab71e5cfe41f20524407351880d4c61dd9c2a69` |

---

## 1. 走查范围与现状基线

| 旅程阶段 | 走过的文件 |
|---|---|
| 首次打开 | `App.tsx`（侧栏/顶栏）、`pages/Dashboard.tsx`、`store.tsx`、`topbarStatus.ts`、`styles.css` |
| 选节点 | `pages/Nodes.tsx`、`pages/Subscriptions.tsx` |
| 连接 | `App.tsx`（模式/连接）、`store.tsx`、`fatalError.tsx`、`ErrorBoundary.tsx` |
| 失败 | `App.tsx:188-229`、`pages/Dashboard.tsx:502-540`、`failure.ts`（已读，设计口径正确） |
| 意图/MITM | `pages/Intent.tsx`（闸门/状态/审计） |
| 排查 | `pages/Logs.tsx`、`pages/Routing.tsx`、`pages/Settings.tsx`（更新与助手节） |
| 日常使用 | `pages/Routing.tsx`（草稿）、`InlineConfirm.tsx` |

**基线套件的盲区（这决定了下面为什么"没有测试会红"）**：

```
$ ls apps/ui/src | grep -iE "focus|a11y|empty|contrast|silent|busy|aria"
(无 a11y/focus/contrast/空态 专用测试文件)
$ grep -rn "aria-live" apps/ui/src --include=*.tsx | wc -l   → 0
$ grep -rn 'role="alert"' apps/ui/src --include=*.tsx | grep -v test
  → 只有 IncidentReport.tsx:113,383 / Logs.tsx:189 / Routing.tsx:332,368,481
```

即：**可访问性、空态/错误态、去重、进度反馈这四类没有任何断言**。465 条测试全绿与它们无关。

---

## 2. 优化清单（按旅程排序；每条给严重度 / `文件:行` / 用户看到什么 / 改法 / 改法性质）

> 严重度：**高** = 用户会得到错误结论、执行不了动作，或数据/隐私受损；**中** = 明显卡顿、困惑或弱势用户不可用；**低** = 打磨。

### 旅程 A · 首次打开

#### A1 ｜ **高** ｜ 空态冒充错误态：读不到数据时显示「正在加载…」或「还没有任何节点」
- **类别**：空状态与错误态混用
- **`文件:行`**：
  - `pages/Dashboard.tsx:189` `if (!snapshot) return <div className="empty">正在加载…</div>;`
  - `pages/Routing.tsx:217`、`pages/Settings.tsx:295` 同上
  - `pages/Nodes.tsx:42` `const nodes = snapshot?.nodes ?? [];` → 落到 `:148-157` 的空态「还没有任何节点。去『订阅』页添加一个机场订阅…」
  - `pages/Subscriptions.tsx:24` `?? []` → `:80-83`「还没有订阅。添加后会自动拉取一次…」
- **用户看到什么**：后端 IPC 挂了（或一直没回），仪表盘/规则/设置永远停在「正在加载…」；节点页和订阅页更糟 —— 它们**不区分"没有"与"没读到"**，直接告诉用户「你还没有任何节点/订阅」，把人引向"去添加订阅"，而真相是数据没读回来。注意 `store.tsx` **只有** `logsLoad` 有 phase（`:55-59`、`:140`），`snapshot` 没有对应状态（`:125`）。
- **改法（可立刻改 / 前端）**：在 `store.tsx` 加 `snapshotPhase: "loading" | "loaded" | "failed"`（`refresh()` 的三个分支各写一次，同 `loadLogs` 的写法）；这五个页面按它分支渲染：
  - `loading` → 「正在读取状态…」
  - `failed` → `banner banner--error` 「读不到状态：{原因}」+ `重试`（调用既有 `refresh()`）
  - `loaded && 空` → 才允许出现「还没有任何节点/订阅」
- **验收**：新增 `apps/ui/src/snapshotPhase.test.tsx`：`snapshot` mock 为 `null` 且 `api.snapshot` reject → 断言 `Nodes` 渲染文本**不含**「还没有任何节点」、**含**「读不到状态」；`Dashboard` 断言**不含**「正在加载…」。

#### A2 ｜ **中** ｜ 当前页 / 当前模式只靠 class 表达，读屏读不出
- **类别**：可访问性（ARIA）
- **`文件:行`**：`App.tsx:148-157`（侧栏 `nav-item`，选中只有 `is-active`）；`App.tsx:358-381`（模式分段按钮，选中只有 `is-active`）
- **用户看到什么**：视觉正常；VoiceOver 用户听到一列 9 个「仪表盘/节点/…」按钮和 3 个「直连/系统代理/TUN」按钮，**不知道自己在哪一页、当前是哪个模式**。
- **改法（可立刻改 / 前端）**：
  - 侧栏：`aria-current={view === item.id ? "page" : undefined}`；
  - 模式组：给每个按钮 `aria-pressed={mode === m}`（或把整组改成 `role="radiogroup"` + `role="radio"`+`aria-checked`，与设置页 tabs 的做法一致）。
- **验收**：`App` 渲染后断言 `document.querySelector('[aria-current="page"]')?.textContent` 与 `getByRole("button", {name:"TUN", pressed:true})`。

#### A3 ｜ **中** ｜ 键盘焦点几乎不可见
- **类别**：可访问性（焦点）
- **`文件:行`**：`styles.css:1329-1332`（全仓**唯一**的 `:focus-visible`，只给 `.set__tab`）；`styles.css:363-375`（输入框规则里 `outline: none`，只用 `border-color` 变化表示聚焦）；`.nav-item`(`:100-135`)、`.btn`(`:255-340`)、`.list__row`(`:450-470`) 均无焦点样式
- **用户看到什么**：macOS 默认只在开启"全键盘控制"时给按钮画焦点环；这个应用没有任何自定义焦点样式 ⇒ 只按键盘的用户**看不到焦点在哪**，无法操作侧栏、顶栏按钮和列表动作。
- **改法（可立刻改 / 前端，一条规则）**：
  ```css
  :focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  ```
  并把输入框那条 `outline: none` 改成 `outline: none;` + `:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }`（保留现有 border 变色）。
- **验收**：新增 `apps/ui/src/focusVisible.test.ts`：断言 `styles.css` 含 `:focus-visible` 且覆盖 `button`/`.nav-item`；手工截图点：Tab 走一遍侧栏 9 项 + 顶栏 3 个模式 + 连接按钮，每一项都要能看到蓝色描边（截图点）。

#### A4 ｜ **中** ｜ `--text-faint` 在 11px 正文上对比度不足
- **类别**：可访问性（对比度）
- **`文件:行`**：`styles.css:9` `--text-faint: #64748b;`；11px 使用点：`:124`（`.nav-item__badge`）、`:132`（`.sidebar__footer`）、`:357`（`.field__hint`），另有 20+ 处（`:433,493,502,667,680,693,790,804,932,1026,1040,1097,1137,1192,1215,1228,1481,1599,1635,1725,1735,1791,1841,1868,1873`）
- **用户看到什么**：把 `#64748b` 与两个背景做 WCAG 相对亮度计算：对 `--bg #0f1420` 是 **3.87:1**，对 `--bg-elevated #161d2c` 是 **3.56:1**；字号 11px ⇒ 需 4.5:1。**低于 AA**，弱视/低质量屏幕上「提示与说明」基本读不清（而这些 hint 里恰好装着"要不要手动设系统代理""证书会不会被撤掉"这类关键限定语）。
- **改法（可立刻改 / 前端）**：把 `--text-faint` 提到 `#94a3b8`（即现有 `--text-dim`，实测 7.2:1 / 6.6:1），并保留 `--text-dim` 与之合并；若想保留三级层次，至少取 `#8b98ad` 以上。**注意** `styles.css:541` 与 `:1868` 把 `--text-faint` 当**背景/描边**用，改色后要顺带看那两处。
- **验收**：扩展既有 `scripts/check-css-tokens.py`（它已经按 bundle 读 token）加一条对比度断言：`--text-faint` vs `--bg`/`--bg-elevated` ≥ 4.5:1，退出码非 0 即红；或在 PR 描述里贴出复算命令与结果。

#### A5 ｜ **低** ｜ 首次打开没有任何引导；而默认模式「已连接」永远不会出现
- **类别**：信息层级
- **`文件:行`**：`topbarStatus.ts:288-299`（系统代理模式的 label 恒为「本地代理入口已就绪」，`sub` 要求手动指向端口）；`App.tsx:403-407`（「已选『系统代理』，点右侧『连接』开始」）
- **用户看到什么**：新用户默认 `system_proxy`，点了连接后**永远看不到「已连接」**，看到的是「本地代理入口已就绪 + 系统代理未被本应用修改：需要手动把浏览器或系统代理指向 127.0.0.1:…」。这句话**是诚实的**，但对第一次用的人是一记闷棍：他以为连上了就完事。
- **改法**：在仪表盘状态区加一条**只在首次看到时出现**的说明（可 `localStorage` 记 `seenSystemProxyHint`，**可立刻改 / 前端**）；更可靠的做法是后端给一个持久化字段（**需后端配合：缺 `settings.has_ever_connected: bool`**，或 `app.first_run: bool`），因为 localStorage 会被"清缓存"清掉、也不能区分"从没连过"和"换台机器"。
- **验收**：mock `snapshot.runtime.running=true && mode==="system_proxy"`，断言首次渲染含「**这个模式不会**自动改系统代理设置」且点击"知道了"后再渲染不再出现。

### 旅程 B · 选节点 / 看订阅

#### B1 ｜ **高** ｜ 节点行的"选中"只能鼠标点，键盘/读屏无法操作
- **类别**：可访问性（键盘 + ARIA）
- **`文件:行`**：`pages/Nodes.tsx:283-307`（`<div className={'list__row node-row…'} onClick={busy ? undefined : onSelect} …>`，无 `role`/`tabIndex`/`onKeyDown`）
- **用户看到什么**：视觉上整行可点；键盘用户 Tab 只能落到行内的「二维码」「删除」，**没有任何办法切换到某个节点**；读屏也读不出"这是一组单选里的哪一项、选没选中"（选中只有左侧色条 + `已选中` 文字标签 `:311`）。
- **改法（可立刻改 / 前端）**：把列表外层 `Nodes.tsx:159` 改成 `role="radiogroup" aria-label="节点"`；行改成 `role="radio" aria-checked={selected} tabIndex={selected ? 0 : -1}` + `onKeyDown`（`Enter`/`Space` 选中，`ArrowUp/ArrowDown` 在组内移动焦点并选中），保留行内按钮并用 `e.stopPropagation()`（已有先例 `:342`）。**不要**给 div 加 `role="button"` —— 它与行内两个按钮会形成"按钮套按钮"。
- **验收**：`apps/ui/src/nodeRowA11y.test.tsx`：渲染 2 个节点 → `screen.getAllByRole("radio")` 长度 2；`fireEvent.keyDown(row, {key:"Enter"})` 后断言 `api.selectNode` 被调用且 `aria-checked` 转移；`ArrowDown` 后焦点落到下一行。

#### B2 ｜ **中** ｜ 忙碌时节点行"看起来能点、点下去没反应"
- **类别**：无声失败
- **`文件:行`**：`Nodes.tsx:286`（`onClick={busy ? undefined : onSelect}`）与 `:300-306`（title 里才解释原因）；样式未加 `aria-disabled`/`cursor`
- **用户看到什么**：测延迟/增删节点进行中，整行仍高亮可点（`:hover` 生效），点下去毫无反应；原因只藏在鼠标悬停提示里 —— 键盘/触摸用户完全拿不到。
- **改法（可立刻改 / 前端）**：`aria-disabled={busy}`（或 `data-busy`）+ `cursor: not-allowed`，并在行首插入可见的 `badge`「操作进行中」；把 `:300-306` 的 title 文案同步成可见文本。
- **验收**：断言 busy 时行有 `aria-disabled="true"` 且页面文本含「操作进行中」。

#### B3 ｜ **中** ｜ 两处「复制」失败是静默的（`console.log` 对用户不可见）
- **类别**：无声失败
- **`文件:行`**：`Logs.tsx:115-125`（`exportLogs` 的 `catch { console.log(text); }`）+ `:166-168`（「复制」按钮）；`Nodes.tsx:222-228`（导出弹窗「复制链接」的裸 `void navigator.clipboard.writeText(...)`，连 catch 都没有）
- **用户看到什么**：点「复制」后界面**没有任何变化**（不提示成功也不提示失败）。剪贴板被拒（窗口未聚焦、权限）时，用户以为复制成功，贴出去是空的 —— 这在本项目已经修过一次（`IncidentReport.tsx:52+` 的 `CopyButton`，失败会给 `role="alert"` + 可手动选中的 textarea，见 `Logs.tsx:230-234` 的注释），但这两处**没有跟着换**。
- **改法（可立刻改 / 前端）**：两处都改用既有 `CopyButton`（`Logs.tsx:234`、`Settings.tsx:1473` 已有用法）：`<CopyButton label="复制" text={...} className="btn btn--ghost" />` 与 `load={() => api.diagnostics()}` 同款。
- **验收**：mock `navigator.clipboard.writeText` reject → 断言页面出现「没有复制成功」且出现可选中 textarea（这条断言可直接复用 `incidentReport.test.tsx` 里的写法）。

#### B4 ｜ **中** ｜ 导出二维码弹窗没有对话框语义、没有焦点管理、Esc 关不掉
- **类别**：可访问性（ARIA + 焦点）
- **`文件:行`**：`Nodes.tsx:175-237`（`<div className="modal" onClick=…><div className="modal__box" …>`）
- **用户看到什么**：视觉上是个弹窗；读屏用户听到的只是普通文字流，Tab 会**跑到弹窗背后的页面**去；按 Esc 无反应，只能精确点到遮罩或「关闭」。
- **改法（可立刻改 / 前端）**：`role="dialog" aria-modal="true" aria-labelledby`（给 `modal__title` 一个 id）；打开时把焦点移到「关闭」，关闭后还给触发按钮；`keydown` 监听 Esc 关闭；背景内容加 `aria-hidden`（或至少把焦点循环限制在弹窗内）。
- **验收**：`nodesModalA11y.test.tsx`：打开后断言 `getByRole("dialog")` 存在、`document.activeElement` 是关闭按钮；按 Esc 后弹窗从 DOM 消失。

#### B5 ｜ **低** ｜ 节点页两类错误横幅不是 live region
- **类别**：可访问性（ARIA）
- **`文件:行`**：`Nodes.tsx:132-137`（`addError`）、`:188-193`（`exportError`）
- **用户看到什么**：读屏用户点完「解析并添加」后**不会被告知失败**（「这次添加没有生效 —— 原因见页面上方的提示条」只在全局横幅里，见 D1/D2）。
- **改法（可立刻改 / 前端）**：两处加 `role="alert"`（与 `Routing.tsx:332`、`Logs.tsx:189` 一致）。
- **验收**：断言 `getByRole("alert")` 存在。

### 旅程 C · 连接

#### C1 ｜ **高** ｜ 忙碌时点「连接」/切模式是**静默无效**
- **类别**：无声失败
- **`文件:行`**：`App.tsx:306-319`（`switchMode`/`toggleRun`）→ `store.tsx:258-274`（`run` 的第一行 `if (busyRef.current) return false;`）→ `App.tsx:321-322,447`（`runBusy` 只认 `busy==="start"|"stop"`，`modeBusy` 只认 `"mode"`）
- **用户看到什么**：正在「测试延迟」或「保存设置」时，顶栏那颗「连接」按钮和三个模式按钮**都是可点的**；点下去既不变文字、也不报错、也没有任何提示 —— 因为 `run()` 在 busy 竞态下直接返回 `false`，调用方 `toggleRun` 不检查返回值。用户得到的是"这个按钮坏了"。
- **改法（可立刻改 / 前端，二选一，建议都做）**：
  1. 把禁用判据从"同操作忙碌"改成"任一操作忙碌"：`disabled={busy !== null || …}`（与设置页/节点页一致），并给 `title` 写清"有操作正在进行"；
  2. 在 `store.tsx` 暴露 `busyLabel: string | null`（把 `busy` 的键名映射成人话："正在测试延迟/正在保存设置/正在重连…"），顶栏渲染一个常驻 `role="status"` 的小徽章 —— 这样其它长操作也顺带有了进度（见 C2）。
- **验收**：mock `api.testLatency` 为未决 Promise → **判据必须是 UI**（`api.start` 在改前也不会被调用：`run()` 在 `busyRef` 检查处就返回了，见 `store.tsx:260`）：断言「连接」按钮 `disabled === true`（改前为 `false`）；若选方案②，断言页面含 `role="status"` 的「正在测试延迟」。

#### C2 ｜ **中** ｜ 三个长操作完全没有进度反馈
- **类别**：无声失败
- **`文件:行`**：
  - `pages/Dashboard.tsx:461-467`「更新全部订阅」：`disabled={busy !== null}`，**没有** `<span className="spin"/>`（同屏的「测试延迟」`:359-366` 有）
  - `pages/Settings.tsx:1229-1245`「检查更新 / 更新核心 / 更新 geo」（网络操作）+ `:1256-1269`「回退」：同样只有 disabled
  - `pages/Subscriptions.tsx:119,222-224` 行内「更新」：`busy` 是布尔，**看不出是哪一行在忙**（「更新全部」有 spin `:96`，单行没有）
- **用户看到什么**：点下去后按钮变灰、文字不变，几秒到几十秒内界面**一动不动**；用户会再点、会以为没点上；订阅页更新单行时全部行的按钮一起变灰，但不知道是谁在跑。
- **改法（可立刻改 / 前端）**：统一加 `<span className="spin"/>` + 文案切换（`{busy === "check-updates" ? "正在检查…" : "检查更新"}`）。行级进度：`store.busy` 目前**只有操作名、没有对象 id** —— 若要在"哪一行在更新"上精确，需要后端/快照或 store 扩一个 `busyTarget?: string`（**需后端配合（若走快照）：缺 `runtime.busy: { op, target }`**；也可以前端临时解决：`Subscriptions` 本地 `const [refreshingId, setRefreshingId] = useState<string|null>()`，**可立刻改**）。
- **验收**：mock `api.refreshSubscriptions` 未决 → 断言按钮里出现 `.spin` 且文本为「正在更新…」；订阅页断言只有被点那行出现 spin。

### 旅程 D · 失败

#### D1 ｜ **高** ｜ 同一次失败在两个横幅里重复出现，动作还不一样
- **类别**：信息重复与自相矛盾
- **`文件:行`**：全局横幅 `App.tsx:188-229`（原因 + `errorSteps` + `errorNameNote` + **最多 3 个动作**）+ 仪表盘最急 notice `Dashboard.tsx:502-539`（`上次运行出错：{同一段原文}` + 同一份 steps + **只有 1 个动作** `failureActions(raw)[0]`）
- **用户看到什么**：连接失败后回到仪表盘，**同一条红字出现两次**（一条在内容区顶端、一条在状态区下方）。两条的动作集合不同：上面有「去重装助手 / 去换一个节点 / 查看日志」，下面只有第一个；上面能「关闭」，下面关不掉、且切页再回来仍在。用户会以为发生了两次不同的故障。
- **改法（可立刻改 / 前端）**：`store.tsx` 已经能区分来源（`fail()` 是唯一出口 `:237-242`），加一个 `errorSource: "command" | "snapshot" | "logs"`；仪表盘的 `last-error` notice 仅在 **`errorSource !== "command"`**（或 `error !== runtime.last_error`）时渲染。更省事的等价判据：在 `App.tsx` 已有 `stripMarkup(error)` 与 `runtime.last_error`，比较规范化后的字符串即可。
- **验收**：`failureDedupe.test.tsx`：mock `api.start` reject（文本 = `runtime.last_error`）→ 渲染含 `Dashboard` 的整壳 → 断言 `.banner--error .banner__reason` 的**数量为 1**，且 `role="alert"` 数量不超过 1。

#### D2 ｜ **高** ｜ 最关键的失败横幅不是 live region（而次要的横幅是）
- **类别**：可访问性（ARIA / 读屏）
- **`文件:行`**：`App.tsx:188-229` 全局失败横幅**无** `role`；`Dashboard.tsx:391-398` 的 notice 容器**无** `role`；`Intent.tsx:386-395`（err）与 `:703-713`（mitmErr）**无** `role`；对照：`Logs.tsx:189`、`Routing.tsx:332/368/481`、`IncidentReport.tsx:113/383` **有** `role="alert"`。另外 `App.tsx:233-243`「已自动恢复连接」也没有 `role="status"`。
- **用户看到什么**：读屏用户点「连接」失败后**听不到任何变化**；自动恢复成功也听不到。而日志页一次读取失败反倒会被播报。这是同一产品里两套标准。
- **改法（可立刻改 / 前端）**：统一规则 —— 错误 → `role="alert"`；信息/完成/警告 → `role="status"`；给四处补齐。注意 `role="alert"` 不要加在高频重渲染的容器上（否则会反复播报）。
- **验收**：`a11yLiveRegions.test.tsx`：断言「连接失败」横幅有 `role="alert"`、「已自动恢复」有 `role="status"`、Intent 的两条错误横幅各有 `role="alert"`。

#### D3 ｜ **高** ｜ 意图页「现在会真的拦吗」不看"待下发"，会答「会」
- **类别**：信息自相矛盾（错误信念）
- **`文件:行`**：`pages/Intent.tsx:416-429`，关键分支 `:425-426`
  ```tsx
  : summary.block_rules > 0
    ? `会：当前有 ${summary.block_rules} 条拦截规则`
  ```
  旁边 `:442-450` 写着「当前规则集合 … （尚未下发到核心）」，`:469-483` 的横幅写着「判决变了但还没下发给核心」。
- **用户看到什么**：开关开、演练关、缓存里已有拦截判决（重启 App 后自动加载缓存就会这样），但**核心还没重连**（甚至没在跑）时，这一行答 **「会：当前有 N 条拦截规则」**；同屏另外两处说"还没下发"。用户相信广告已经被拦了。`summary.block_rules` 只是 `intent.rs::rules()` 的**当前应生效集合**，与核心是否加载无关。
- **改法（可立刻改 / 前端，字段已有 `rules_pending_apply`）**：
  ```tsx
  : summary.block_rules > 0
    ? summary.rules_pending_apply
      ? `还不会：${summary.block_rules} 条拦截规则已就绪，但还没下发到核心 —— 点下面的「应用（会重连一次）」才生效`
      : `会：核心已加载 ${summary.block_rules} 条拦截规则`
    : "暂时不会：当前 0 条拦截规则"
  ```
  （并建议把这一行的判据抽成纯函数导出，配单测 —— 它现在是页面里最容易再犯的错。）
- **验收**：`intentHonesty.test.tsx`：mock `summary = {block_rules: 3, rules_pending_apply: true, drill: false}` → 断言这一行的文本**以「还不会」开头**且含「还没下发到核心」；`rules_pending_apply:false` 时才允许匹配 `/^会：/`。

#### D4 ｜ **中** ｜ 后端失败文案是"带记号的散文"，前端只能猜结构
- **类别**：架构性（导致重复与错配）
- **`文件:行`**：门禁文案 `apps/desktop/src/supervisor.rs:459-462`（含 `**` 与 `\n`）；前端补救 `failure.ts:265-267`（`stripMarkup`）、`:285-290`（`plainOneLine`）、`:184-210`（按**正则线索**猜动作）
- **用户看到什么**：现在显示是对的了（记号擦掉、换行保留），但"该给什么下一步"是靠**中文正则猜**的（`/helper|助手/` → 重装助手…），后端换一个词就会错配或漏配。这不是本轮必须修的用户可见缺陷，但它决定了后续每一条文案都要两边对齐。
- **改法（需后端配合）**：后端在 `runtime` 里补 `last_error_kind`（枚举：`helper_protocol_mismatch` / `gate_probe` / `local_port_in_use` / `core_missing` / …），并让后端文案不含 `**`；前端 `failure.ts` 优先按 kind 选动作，正则只做兜底。**缺的字段：`runtime.last_error_kind`（当前只有 `last_error: String`）。**
- **验收**：`failureHonesty.test.tsx` 增加断言：给定 `last_error_kind` 时动作选择不经过正则（例如故意把文案换成不含关键词的句子，动作仍正确）。

### 旅程 E · 排查

#### E1 ｜ **低** ｜ 日志搜索框没有可访问名
- **类别**：可访问性
- **`文件:行`**：`Logs.tsx:152-158`（`placeholder="过滤关键字"`，无 label/aria-label）；同样 `Nodes.tsx:88-93`
- **用户看到什么**：placeholder 只在空值时可见，读屏把它当提示而非名称；输入内容后该控件的名称就空了。
- **改法（可立刻改 / 前端）**：`aria-label="过滤日志关键字"` / `aria-label="搜索节点"`。
- **验收**：`getByRole("textbox", {name:"过滤日志关键字"})`。

#### E2 ｜ **低** ｜ 审计「为什么」按钮没有加载/失败态
- **类别**：无声失败
- **`文件:行`**：`Intent.tsx:991-1002`（点击 → `api.intentExplain(...).then(setExplain).catch(e => setErr(...))`）
- **用户看到什么**：点了没反应（等 IPC），失败时错误出现在**页面顶端**的 `err` 横幅 —— 距离那个按钮很远，用户未必会往上看。
- **改法（可立刻改 / 前端）**：按钮级三态（`idle/loading/failed`），失败在按钮旁显示一行短原因；`err` 横幅只用于页面级失败。
- **验收**：mock reject → 断言按钮邻近文本含原因。

#### E3 ｜ **低** ｜ 崩溃证据（panic.log）这条已经做对了，需要加锁防止回退
- **类别**：（正例）
- **`文件:行`**：`Logs.tsx:243-262`（常驻一行 + 「打开数据目录」走 `runVoid`）+ `:44-57`（路径常量与来源注释）
- **改法**：不需要改。**需要一条断言**：`expect(text).toContain("panic.log")` 且按钮存在 —— 这类"用户不知道证据在哪"的问题修过一次很容易在改版里被挤掉。
- **验收**：`logsPanicHint.test.tsx`。

### 旅程 F · 日常使用

#### F1 ｜ **高** ｜ 规则页未保存的草稿在切换页面时**静默丢失**
- **类别**：无声失败（数据丢失）
- **`文件:行`**：`Routing.tsx:180-185`（`draft` 是组件内 state）+ `:244-251`（只有点保存才落盘）+ `:609-614`（只在同屏提示"有未保存的改动"）；`App.tsx:244-252` 只渲染当前 view ⇒ 切到「节点」再回来，组件卸载、`draft` 归零
- **用户看到什么**：编辑了 3 条规则、切到「日志」看一眼、切回来 —— **改动全部消失**，没有提示、没有恢复。这是整个走查里唯一会**直接丢掉用户工作**的问题。
- **改法**：
  - **可立刻改（前端）**：把 `dirty` 提到 `store`（或 App 级 context），侧栏/顶栏导航在 `dirty` 时走二次确认（复用 `InlineConfirm` 的问句样式："有未保存的规则改动，离开会丢失"），或把 `draft` 提升到 App 级保持；
  - **需后端配合的部分**：要覆盖 macOS 关窗/⌘Q 的话，需要托盘/窗口的退出前钩子（当前 `tray.rs` 有 `shutdown_and_exit`，但没有"前端有未保存改动"的通道）—— **缺一个"退出前询问"的 Tauri 事件或 `settings.dirty` 异步查询**。建议本轮只做前端导航守卫（覆盖 90% 场景），退出场景写进 task-23 的"不做"清单。
- **验收**：`routingDraftGuard.test.tsx`：改一条规则 → 点击侧栏「日志」→ 断言出现确认问句，选择"留着"则仍停留在规则页且草稿还在；选择"放弃"才切页。

#### F2 ｜ **中** ｜ `InlineConfirm` 展开后焦点丢在已卸载的按钮上
- **类别**：可访问性（焦点）
- **`文件:行`**：`InlineConfirm.tsx:80-95`（未确认态按钮）→ `:97-118`（确认态整块替换）—— 没有 `useRef`、没有 `focus()`
- **用户看到什么**：键盘用户 Tab 到「删除」、按 Enter → 原来的按钮被卸载，**焦点掉到 `<body>`**；接着按 Tab 会从页面开头重新走一遍，Esc 虽然能取消（`:67-74`）但焦点回不到原处。破坏性操作尤其需要焦点可预期。
- **改法（可立刻改 / 前端）**：`armed` 变 true 时 `confirmBtnRef.current?.focus()`；确认/取消后把焦点还给触发按钮（存一个 `triggerRef`）。
- **验收**：`inlineConfirmFocus.test.tsx`：点击「删除」→ 断言 `document.activeElement` 是「确认删除」；按 Esc → 断言焦点回到原「删除」按钮。

#### F3 ｜ **中** ｜ 关键说明只放在 `title` 里，键盘与触摸拿不到
- **类别**：可访问性（信息可达性）
- **`文件:行`**：`App.tsx:365-377`（模式按钮的完整含义在 tooltip："只开本地 SOCKS/HTTP 入口…需要你手动把浏览器或系统代理指向它"）；`Dashboard.tsx:333`（更新 chip 的完整口径在 `title`）；`Nodes.tsx:325-334`（距离/可用徽章的解释在 `title`，节点页注释 `:6-10` 自己也承认"提示只有鼠标悬停才看得到"）
- **用户看到什么**：`title` 在 macOS 上要悬停 1 秒以上才出现，键盘和触摸拿不到；这恰好是"系统代理模式到底做了什么"这种**决策必需**的信息。
- **改法（可立刻改 / 前端）**：把**行动必需**的那一句从 `title` 移进可见文本或 `aria-describedby` 指向的 `.sr-only`；纯补充信息可以留在 title。优先级：模式按钮 > 更新 chip > 节点徽章。
- **验收**：断言模式按钮的 `aria-describedby` 指向的文本含「需要手动」，或可见文本里有这一句。

#### F4 ｜ **低** ｜ 意图页 MITM 名单只在 `onBlur` 提交
- **类别**：无声失败
- **`文件:行`**：`Intent.tsx:795-809`（`onChange` 只改本地 `domainsText`，`onBlur` 才 `patchMitm`）
- **用户看到什么**：输入域名后直接 ⌘Q 或用托盘退出（不触发 blur）→ 名单**没保存**，下次打开还是旧的。
- **改法（可立刻改 / 前端）**：加一颗显式「保存名单」按钮（或 300ms debounce 自动保存 + "已保存"提示），并把"未保存"状态说可见。
- **验收**：断言输入后无需 blur 也会调用 `api.saveSettings`（若走 debounce 则用 `vi.useFakeTimers`）。

---

## 3. 汇总表

| ID | 严重度 | 类别 | 一句话 | 改法性质 |
|---|---|---|---|---|
| A1 | 高 | 空态/错误态 | 读不到数据时显示「正在加载…」或「还没有任何节点」 | 前端 |
| C1 | 高 | 无声失败 | busy 时点连接/模式静默无效 | 前端 |
| D1 | 高 | 重复矛盾 | 同一失败两个横幅、动作数不同 | 前端 |
| D2 | 高 | 可访问性 | 失败横幅无 `role="alert"`（次要横幅有） | 前端 |
| D3 | 高 | 矛盾（错误信念） | 意图页答「会拦」而尚未下发 | 前端（字段已有） |
| B1 | 高 | 可访问性 | 节点选中只能鼠标 | 前端 |
| F1 | 高 | 无声失败 | 规则草稿切页静默丢失 | 前端（退出场景需后端/Tauri） |
| A2 | 中 | 可访问性 | 当前页/模式读屏读不出 | 前端 |
| A3 | 中 | 可访问性 | 焦点几乎不可见 | 前端（CSS） |
| A4 | 中 | 可访问性 | `--text-faint` 11px 对比度 3.56–3.87:1 | 前端（CSS） |
| B2 | 中 | 无声失败 | busy 时行假可点 | 前端 |
| B3 | 中 | 无声失败 | 两处「复制」静默 | 前端 |
| B4 | 中 | 可访问性 | 导出弹窗无 dialog/焦点/Esc | 前端 |
| C2 | 中 | 无声失败 | 订阅刷新/检查更新无进度 | 前端（行级目标可加 store 字段） |
| D4 | 中 | 架构 | 后端文案散文 → 前端正则猜动作 | **需后端**：`runtime.last_error_kind` |
| F2 | 中 | 可访问性 | 二次确认丢焦点 | 前端 |
| F3 | 中 | 可访问性 | 决策信息只在 `title` | 前端 |
| A5 | 低 | 信息层级 | 首启无引导 / 系统代理模式无「已连接」 | 前端近似；精确判据**需后端** `has_ever_connected` |
| B5 | 低 | 可访问性 | 节点页错误横幅无 role | 前端 |
| E1 | 低 | 可访问性 | 搜索框无名称 | 前端 |
| E2 | 低 | 无声失败 | 审计「为什么」无加载/就地表错 | 前端 |
| E3 | 低 | 正例 | panic.log 引导已做对 → 加回归锁 | 前端（仅测试） |
| F4 | 低 | 无声失败 | MITM 名单只在 blur 提交 | 前端 |

类别覆盖自查：重复与矛盾 D1/D3；无声失败 C1/C2/B2/B3/E2/F1/F4；空态与错误态 A1（B1 的行空态复用同一判据）；可访问性 A2/A3/A4/B1/B4/B5/D2/E1/F2/F3。

---

## 4. 优先级前 5 条 + 每条怎么证明改好了

> 排序依据：**先修会造成错误结论或丢失用户工作的**，再修让弱势用户不可用的。前 5 条全部是「可立刻改（前端）」，不阻塞在后端字段上。

| 顺序 | ID | 为什么排这里 | 怎么证明改好了（断言 / 核对点） |
|---|---|---|---|
| 1 | **D3**（意图页「现在会真的拦吗」） | 唯一的**当前仍存在**的"把没发生说成已发生"；一行判据的事，改完立刻消除错误信念 | 新增 `intentHonesty.test.tsx`：`{block_rules:3, rules_pending_apply:true}` ⇒ 文本以「还不会」开头且含「还没下发到核心」；`rules_pending_apply:false` ⇒ 匹配 `/^会：/`。**并**把判据抽成导出纯函数（`intentVerdictLine(summary) ??`），单测直接钉 |
| 2 | **A1**（空态冒充错误态） | 影响每一页的冷启动/失败首屏；现在的行为是**给错因**（"你没有节点"） | 新增 `snapshotPhase.test.tsx`：`snapshot=null` + `api.snapshot` reject ⇒ Nodes 不含「还没有任何节点」且含「读不到状态」；Dashboard 不含「正在加载…」；`snapshot` 正常返回空数组时**才**出现「还没有任何节点」 |
| 3 | **D1+D2**（失败横幅去重 + live region） | 一屏两条红字、动作不一致，且读屏用户完全收不到失败 | `failureDedupe.test.tsx`：`api.start` reject ⇒ `.banner--error .banner__reason` 数量为 1；`a11yLiveRegions.test.tsx`：失败横幅 `getByRole("alert")` 存在，`recoveredAttempt` 横幅 `role="status"` |
| 4 | **C1**（busy 时按钮静默无效） | "点了没反应"是最典型的无声失败，且发生在主操作按钮上 | `busyGuard.test.tsx`：mock `api.testLatency` 未决 ⇒ 「连接」按钮 `disabled === true`（改前为 `false`；`api.start` 改前改后都不会被调用，**不能**用它当判据，见 `store.tsx:260`）；若加 `busyLabel` 徽章，断言页面含 `role="status"` 的「正在测试延迟」 |
| 5 | **F1**（规则草稿切页丢失） | 唯一会丢用户工作的问题 | `routingDraftGuard.test.tsx`：改一条规则 → 点侧栏「日志」⇒ 出现确认问句；选"留着"仍停在规则页且草稿仍在；选"放弃"才切页 |

**其余中/低项的证明方式**（每条都有对应断言点，见第 2 节每条的「验收」）：A2 `aria-current`/`aria-pressed` 断言；A3/A4 需要截图点 + 把对比度检查接进 `scripts/check-css-tokens.py`；B1 `getAllByRole("radio")` + `keyDown` 断言；B3 复用 `incidentReport.test.tsx` 的剪贴板失败写法；B4 `getByRole("dialog")` + Esc；C2 `.spin` 与文本断言；F2 焦点断言；F3 `aria-describedby` 断言；F4 `vi.useFakeTimers`。

---

## 5. 工具链恢复后要跑什么（本轮**没有**跑过这些）

⚠️ 前提更正见 §0.2：本机实测 git/clang/cargo/node/vitest **都在**，不是"不可用"。因此下面这份清单不是"等工具链"，而是**等 task-23 的改动落地后**要按顺序跑的东西。本轮我只跑了第 0 步（改动前的基线）。

0. **基线（本轮已跑，仅作对照）**：`cd apps/ui && ./node_modules/.bin/vitest run` → `43 files passed / 465 passed + 1 todo`。改动后**同一个数字只许增不许减**。
1. `cd apps/ui && ./node_modules/.bin/vitest run src/<每个新增的 test 文件>` —— 本文 §4 每条要求的断言；先单跑，确认它们**在改动前是红的**（否则断言没钉住东西）。
2. `cd apps/ui && ./node_modules/.bin/vitest run` —— 全量；重点看新增的可访问性/空态用例是否与既有 43 个文件互相污染（`test-setup.ts` 会重置 DOM）。
3. `cd apps/ui && ./node_modules/.bin/tsc --noEmit`（与 `apps/ui/package.json` 的 `build` 同一口径；`tsconfig.json` 在 `apps/ui/` 下）—— 断言里用到的 `aria-*`/`role` 属性与 `MitmStatus`/`IntentSummary` 字段必须过类型。
4. `python3 scripts/check-css-tokens.py` —— 若按 A4 扩展了对比度断言，这里必须退出码 0；同时确认改 `--text-faint` 没打破 token 引用检查（`:541`、`:1868` 两处把它当背景/描边用）。
5. `bash scripts/check.sh` —— 仓库总门禁（含 site/构建检查）。**注意**：它可能会拉起 `cargo`，本轮没有跑；若在冻结窗口内，按 `docs/verification/BUILD-LOCK.md` 的流程先确认允许。
6. 若 task-23 碰了 `apps/desktop/**`（D4 的 `last_error_kind`）：`cargo test -p xraytun-desktop`（或根 `cargo test`）+ `cargo clippy`（与既有 CI 口径一致）。
7. **手工核对点（无自动化，必须截图）**：
   - A3：Tab 遍历侧栏 9 项 + 顶栏 3 个模式 + 连接按钮，每项都要看到蓝色焦点描边；
   - A4：把系统"降低对比度/增强对比度"打开各截一张，确认 `.field__hint` 可读；
   - B1：只用键盘完成"选一个节点并连接"；
   - F1：编辑规则 → 切页 → 返回，确认草稿在（或确认问句）；
   - D2：开 VoiceOver，连接失败时能听到播报。
8. **提交**：`git -c commit.gpgsign=false commit -q --only docs/verification/UX-OPTIMIZATION-PLAN.md -F <msg>`（本轮的提交口径；**绝不 `git add -A`、绝不 push**）。

---

## 6. 我没能验证的

- **没有任何改法被实现或验证**。本文是分析与清单；§4 的"验收判据"是**待实现后要跑的断言**，不是结果。唯一真实跑过的是改动前基线（43 文件 / 465 passed + 1 todo），它**不能**证明清单里任何一条已经存在。
- **没跑真机 GUI / VoiceOver**。`role`/`aria`/焦点/对比度的结论全部来自读源码与 CSS 计算；`--text-faint` 的 3.87:1 / 3.56:1 是我按 WCAG 相对亮度公式手算的（`#64748b` vs `#0f1420` / `#161d2c`），**没有**用浏览器取计算样式复核，也**没有**在 VoiceOver 下走过一遍。
- **没验证 task-23 的改动会落在哪个提交上**。`7ddda64` 是本文写作时的基准；`apps/ui/**` 正在被其它队友快速推进（写作期间 HEAD 从 `e735e22` → `7ddda64` → `bde1c5d`）。已核对本文引用的 12 个文件在 `7ddda64` 与 `bde1c5d` 之间 byte-identical；若之后漂移，请用 §0.3 的 blob 对齐。
- **没读全部 UI**。`Globe.tsx`(867)、`Topology.tsx`(206)、`topology/*`（约 1400 行）、`IncidentReport.tsx`(410)、`Settings.tsx` 全文（只读了 tabs/save/update/helper 相关段）没有逐行走查；`failure.ts` 只做了口径审读（它的线索正则与 `failureHonesty.test.tsx` 没有逐条复算）。
- **没验证 D4 的后端改动成本**。`runtime.last_error_kind` 是新字段，我没有读 `apps/desktop/src/state.rs` / `snapshot.rs` 的全部失败写入点来评估改动面；这个建议只到"需要哪个字段"的粒度。
- **`styles.css:541` / `:1868` 把 `--text-faint` 当背景色用的具体影响**没有肉眼确认（A4 的改色需要顺带看那两处，我标注了但没出图）。
