# task-123 · 自动报错链端到端独立验收（task-130 后端 + task-131 前端）

> 验收人：`tester`（独立验证，不采信实现者自报）
> 绑定版本：`11d4af5`（HEAD；验收过程中从 `c5b2dfe` 前移过一次，见 §1.1）
> 判定：**通过（ACCEPT）** —— 未发现静默失败、假承诺或隐私泄漏；附 4 条缺口（§4，均不阻断）
> POST 预算：本卡**用 1 次**（上限 3）；端点对象数**验收前 0、验收后 0**（§2.2）

---

## §0 口径与边界（先说清哪些是「我量到的」）

| 类别 | 内容 |
|---|---|
| **我量到的** | 真实端点上的一次上传/读取/删除全链（原始 HTTP 状态码与响应体）；R2 对象数的前/后快照；本地隐私闸对造出的脏包的原始输出；两次测试套件的真实计数；三次变异探针的变红用例名；被拒上传的原始错误文本 |
| **实现者说的（我另找证据或明确标注未复核）** | task-130/task-131 的完成报告本身**不构成证据**；我用「读断言 + 变异探针 + 独立真相源」替代 |
| **我推断的** | 真机 WKWebView 内的行为（本环境无法自动化）；打包后 `.app` 的 `resource_dir()` 解析结果；无网环境下哨兵照常记录（静态调用图证据，非运行证据） |

**只读纪律**：本卡规定只读 `apps/**`。§3 的三个变异探针需要临时改一行源码，做法是**同一次 bash 调用内** `cp` 备份 + `trap restore EXIT` 还原，并在还原后逐字节校验（sha256 相同）且 `git status --porcelain` 为空（原始输出见 §3.4）。最终两条套件在还原后的树上重跑全绿（§1.2）。

**真机不可自动化**：本机无法 headless 驱动 WKWebView，因此 item 1 与 item 5 的「界面」部分用**组件级断言 + 变异探针**替代，替代覆盖不到的部分逐条写在 §2.1/§2.5 的「测不到」里 —— 不作为「已验证」。

---

## §1 版本绑定

### 1.1 修订

* 验收主体 HEAD = **`11d4af5`**（`docs(verification): v0.8.35 发布验收档补齐…`）。
* 期间从 `c5b2dfe` 前移：`git log --name-only c5b2dfe..HEAD` 只列了 `docs/verification/RELEASE-v0.8.35.md` 与 `docs/verification/verify-app-bundle-resources.sh`，**不含任何被验工件** ⇒ 不影响本报告结论。
* 验收结束时 `git status --porcelain` 为空（干净树）。

被验工件 sha256（验收后复核，与验收时同字节）：

```
ded839f10cde0a439a3b6bc8e0b35821a58abb1cee91f4208143c0fcc1c8ccd3  apps/desktop/src/commands/incident.rs
7e09bc07b51a9dd50c5d5dc6446be1f8c77f2ef273795372960cc4d601e05960  apps/ui/src/IncidentReport.tsx
ed1c48f4c62bb2097c2e18c9c265226fa71e9f245988f4d6fff80bd145f1c572  apps/ui/src/incident.ts
50e05b49fbf126eb08f67e0f7e63c2dd5c8ff83d3e9e9dfef519f9265725e2d9  apps/ui/src/incidentReport.test.tsx
6969e38b5355bab40df30a0eaf492a1a559c28f63e85a38040d501abeac5c353  scripts/triage-incident.py
7662c2193a9e6bf7cb511512001c328b0c8d8e40250ae27aceeb5e1e058afcb9  scripts/incident-bundle.sh
```

### 1.2 测试基线（还原后重跑的最终态）

```
UI   : Test Files 1 passed (1) | Tests 16 passed (16)
Rust : test result: ok. 17 passed; 0 failed; 1 ignored; 204 filtered out
```

那 1 个 `ignored` 就是手动验收工具 `real_upload_acceptance`（§2.2 用它跑了真链）。

---

## §2 六项逐条验收

### item 1 · 可复现路径：点击 → 出包 → **清单先在界面出现** → 确认 → id → 复制（含剪贴板被拒反例）

**源码结构**（`apps/ui/src/IncidentReport.tsx`）：`collect()`（167-185，只调 `incidentPreview`）与 `confirmUpload()`（187-199，先 `if (!preview) return` 再调 `incidentUpload`）是**两个互不相通的 handler**；`useEffect`（163-165）只加载哨兵计数，**不采集**。

**逐条读断言（不采信测试名）**：

| 卡上要求 | 用例 | 断言的实质 |
|---|---|---|
| 未点击时无清单、无「确认上传」、零次上传 | `incidentReport.test.tsx:95` | `queryByRole("确认上传")===null`、`queryByText("包内清单")===null`、`incidentUpload` **未调用** |
| 点一下只 preview，清单先出，「确认上传」后出 | `:102` | `incidentPreview` 恰 1 次、`incidentUpload` **未调用**、清单三件套在 DOM（`logs/app.jsonl`/`state.json`/「已抹掉订阅 URL 里的凭据」/「因为太大被截断」），**然后**才 `findByRole("确认上传")` |
| 反例：preview 失败 ⇒ 不给确认、不上传 | `:119` | `findByRole("alert")` + 无确认按钮 + `incidentUpload` 未调用 |
| 上传成功 ⇒ 显示 id + 「复制编号」 | `:136` | `incidentUpload` 收到 `preview.bundle_path`；渲染 `inc-201`；有「复制编号」按钮；`received_at` 真解析（不是永远不显示） |
| 反例：时间解析不出来 ⇒ 不编时间但保留 id | `:147` | 无「服务器时间」，id 仍在 |
| 剪贴板被拒 ⇒ `role=alert` + 可手动选中的 `textarea` | `:206` | alert 含「复制失败」「剪贴板不可用」；`box.value==="inc-201"`；`readOnly===true`；**不出现**「已复制到剪贴板」 |
| 反例：剪贴板成功 ⇒ `role=status` | `:220` | 出现「已复制到剪贴板」，无 alert、无手动文本区 |
| 取文本失败也可见 | `:229` | alert 含「生成要复制的文本失败」「读日志失败」 |
| 设置页旧入口也接上可见反馈 | `:240` | 同上（`pages/Settings` 真实组件） |

**有牙证据**：变异 U1（挂载即自动采集）⇒ 8 条红；变异 U2（静默吞掉剪贴板失败）⇒ 恰好 2 条剪贴板用例红（§3）。

**测不到（如实声明）**：真 WKWebView 的 `navigator.clipboard` 权限模型与窗口聚焦行为、真 Tauri IPC 的 `invoke` 序列化、打包 `.app` 里 `resource_dir()` 到脚本的解析。本环境用 vitest + 替身，**不是**真机 GUI 证据。

### item 2 · 一次真实上传 + manifest（不含日志正文）+ 令牌删除复核 404 + 桶不留测试包

包在**哨兵允许的目录**内生成（见 §2.3 的路径自证）：

```
$ ./scripts/incident-bundle.sh --out "$TMPDIR/xraytun-incident/xraytun-incident-clean-20260923T075844Z.zip"
clean: 190077 B  sha256 3b5d67e2a9643e2eac868c66e16f75b78c739586a83c3cbfd52baeaa1f1b1df6
```

App 自身路径（`upload_with` → 本地闸 → `/usr/bin/curl` → 真实端点）：

```
$ XT_T130_BUNDLE=…/xraytun-incident-clean-20260923T075844Z.zip \
    cargo test -p xraytun-desktop --lib real_upload_acceptance -- --ignored --nocapture
UPLOAD_OK id=INC-20260923-075904-913c bytes=190077 received_at=2026-09-23T07:59:04.257Z \
  sha256=3b5d67e2a9643e2eac868c66e16f75b78c739586a83c3cbfd52baeaa1f1b1df6
test result: ok. 1 passed; 0 failed; 0 ignored; 221 filtered out
```

* **字节一致**：我本机算的 `3b5d67e2…` == 服务端返回的 `3b5d67e2…`，且 `bytes` == 本机文件大小 190077 ⇒ 端到端未被改写。

manifest 读取（`GET https://xraytun.top/api/incident/<id>`）：

```
HTTP=200 bytes=266
content-type: application/json; charset=utf-8   cache-control: no-store
x-robots-tag: noindex                            server: cloudflare
.schema_version: int 1        .id: str 24 chars :: INC-20260923-075904-913c
.sha256: str 64 chars :: 3b5d67e2…            .bytes: int 6 chars :: 190077
.content_type: str 15 chars :: application/zip .received_at: str 24 chars :: 2026-09-23T07:59:04.257Z
.retention_days: int 2 chars :: 30
体检：ZIP 魔数出现=False  含 vless:// =False  含 SECRETPBK =False  body=266 B
```

⇒ **不含日志正文**（无 ZIP 魔数、无包内任何内容），字段与 App 返回自洽，保留期 30 天。

删除与复核（**运维要点：头是 `X-Auth-Token`，不是 `Authorization: Bearer`**）：

```
① 无令牌 DELETE            → HTTP=401 {"error":"unauthorized","message":"需要 X-Auth-Token"}
② Bearer 令牌 DELETE       → HTTP=401（同一响应；不给穷举者额外信息）
③ X-Auth-Token DELETE      → HTTP=200 {"id":"INC-20260923-075904-913c","deleted":true,
                                "deleted_at":"2026-09-23T07:59:28.886Z"}
④ 再 GET 同一 id           → HTTP=404 {"error":"not_found","message":"没有这个 id"}
⑤ R2 只读对象数            → count 0
```

R2 快照（独立真相源，不依赖 App 自报）：**验收前 `count 0` → 上传后 `count 2`（`<id>.zip` + `<id>/manifest.json`）→ 删除后 `count 0`**。测试包无残留。

### item 3 · 隐私闸双向验证（脏包必须在上传前被本地闸拦下）

造脏包（在干净包基础上塞一行含假凭据的文本）：

```
leak.txt = vless://11111111-2222-3333-4444-555555555555@node.example.com:443?pbk=SECRETPBK&type=tcp#leak
dirty: 190297 B  sha256 5fabdaec39b09abc69f0b3e8fa45babb3c6cca2ebc15cf9f8849e6fdba12f997
```

**闸 A（我直接跑 App 会跑的同一条命令）**：

```
$ python3 scripts/triage-incident.py --privacy-check …/xraytun-incident-dirty-20260923T075844Z.zip
隐私闸：扫描 …（7 个文件）
✗ 命中 3 处疑似密钥/隐私模式 —— **fail closed（退出码 1）**，请先修脱敏再上传：
    leak.txt:1:uuid    1111…（len=36）
    leak.txt:1:uri-secret-param    SECR…（len=9）
    leak.txt:1:proxy-url-with-credentials    vles…（len=93）
GATE_EXIT=1
```

**闸 B（走 App 自身路径）**：

```
真实上传失败：SecretDetected { message: "本机隐私闸拦下了这个包：命中 3 处（只给位置与类型）
 ⇒ 已 fail closed，**没有上传**。请先在本地脱敏后重试。",
 hits: [uuid@leak.txt:1, uri-secret-param@leak.txt:1, proxy-url-with-credentials@leak.txt:1] }
```

* 错误是**类型化**的 `SecretDetected`（不是笼统的「上传失败」），`hits` 只带 `file/line/kind`，**不含原值**。
* 界面口径：`apps/ui/src/incident.ts` 的 `kind==="secret"` 分支渲染「包内检测到疑似密钥 —— **已阻止上传**（没有发出任何数据）」+ `文件:行号:类型`；用例 `:161` 断言 alert 含「已阻止上传」「logs/app.jsonl:42:uuid」且**全文（含 `document.body`）都不出现**假密钥。
* 有牙证据：变异 U3（把该文案改成泛化的「上传失败。」）⇒ 恰好这条用例变红（§3）。
* 顺序证据：`upload_with` 里闸在 curl 之前（`incident.rs:465` 附近的 `privacy_gate(...)` 先于构造 curl 参数），且脏包跑完后 R2 对象数仍为 **0**（§2.4 的独立真相）。

**★ 附带发现（真实防火墙）**：第一次我用 `/tmp/xraytun-incident/dirty.zip` 跑，App **在跑闸之前**就拒了：

```
Server { code: 0, message: "拒绝上传：/tmp/xraytun-incident/dirty.zip 不是本机刚生成的现场包
（只允许临时目录 /var/folders/68/…/T/xraytun-incident 下的 xraytun-incident-*.zip）" }
```

`bundle_path_is_ours()`（`incident.rs:424`）要求 `绝对路径 ∧ 前缀 == temp_dir()/xraytun-incident ∧ 名字 xraytun-incident-*.zip` ⇒ 「拿这条命令当任意文件外发通道」被堵住。macOS 上 `/tmp` 不是 `temp_dir()`（后者是 `/var/folders/…/T`），所以 **`incident.rs:1062` 文档注释里给的 `XT_T130_BUNDLE=/tmp/…` 配方照抄会被拒**（§4 F-1）。

### item 4 · 不许静默上传（未点确认前没有任何 POST）

三条互补证据：

1. **UI 没有自有网络出口**：`grep -rnE "fetch\(|XMLHttpRequest|navigator\.sendBeacon|xraytun\.top" apps/ui/src --include=*.ts --include=*.tsx`（排除测试）**零命中** ⇒ 前端唯一出口是 Tauri `invoke`。
2. **组件级哨兵 + 有牙**：`:95`（进入时零上传）、`:102`（点「报告问题」只 preview，`incidentUpload` 未调用）、`:119`（preview 失败也不上传）；变异 U1（挂载即自动 `collect()`）使 **8 条**用例变红 —— 说明「未点击不出包/不联网」这条红线是被**真的断言住**的，不是靠实现者自觉。
3. **独立真相源**：脏包跑完（App 路径）后 R2 对象数仍 `count 0` ⇒ 本地闸命中时**确实一个字节都没出门**。

**出包本身也不联网**：`scripts/incident-bundle.sh` 内无 `curl/wget/urlopen/requests`（第 139 行的 `https://sub.example.org/...TOPSECRET` 是**脱敏自测 fixture**，不是网络调用）。

**残差（如实声明）**：我没有做包级抓包（pcap）。上面的替代是「替身计数（独立于 UI 自身状态）+ R2 对象数（服务端真相）+ 静态出口审计」；三者一致，但不等于在网卡上看到 0 个包。

### item 5 · 哨兵：真实异常 ⇒ 本地文件多一条 + 界面角标 +1；离线也照常记录

**触发链（源码，生产用）**：

```
apps/desktop/src/commands/diagnostics.rs:14  pub async fn tail_logs(...)      ← 日志页刷新
   └─ 读侧丢行 ⇒ loss_warning(stats)                                          (:105)
      └─ LossNotify 去重（同一签名只提醒一次，避免刷爆日志）                    (:39-49)
         └─ incident::record(&state, "log_read_loss", "warn", …)              (:50)   ← 卡上举的那类异常
apps/desktop/src/commands/core.rs:681        record(state, "watchdog_invalidated", "warn", …) ← 另一真实触发点
```

**写入是纯本地**：`record`（`incident.rs:523`）→ `append_anomaly`（`:494`）只用 `std::fs::OpenOptions` 追加 JSONL；失败只留 `tracing::warn`，**不影响主流程**。调用图上没有网络 ⇒「离线照常记录」在代码层面成立（**静态证据**，非断网运行证据）。

**读侧**：`incident_anomaly_count`（`:598`）→ `read_anomalies`（`:508`，坏行跳过、取最近 N 条）；命令已注册（`apps/desktop/src/lib.rs:119`）。文件位置是契约：`logs/anomalies.jsonl`（`:489`）。用例 `anomalies_append_increment_and_read_back`（2 条 ⇒ 计数 2、取最近 1 条得最后一条）与 `anomalies_skip_malformed_lines_but_keep_the_rest`（1 条坏行 ⇒ 其余保留）都在 §1.2 的 17 passed 里。

**界面**：`loadCount()` 在挂载时读一次（`:163-165`），上传成功后重读（`:195`）；读不到/命令缺席时**不显示角标也不崩**（`:154-161` + 用例 `:275`）。用例 `:267` 断言「计数 3 ⇒ 显示『有 3 条待上报』」且**不触发采集或上传**；`:261` 断言计数 0 不显示角标。

**测不到 / 缺口**：**角标不轮询、也不订阅事件** ⇒ 页面开着时新写入的异常不会自动 +1（要重挂载或上传成功后才刷新，见 §4 F-3）。真机「文件 n 条 → 运行中 GUI 角标 n」的联合链路未验（无 headless GUI）；我验的是「写/读/渲染」三段的组件级契约 + 生产源码接线。

### item 6 · 对抗性：把 `scripts/incident-bundle.sh` 移走 ⇒ App 必须明确报错，不许假装出包成功

**命令层前置拦截**（比单测更强的一层，实现在命令入口）：

```
incident.rs:549-555   let missing = tools.missing();
                      if !missing.is_empty() { return Err(format!(
                          "随包缺少脚本：{} —— 现场包无法生成（这不是「包已生成」）。", …)) }
incident.rs:579-585   incident_upload 同款拦截，消息「随包缺少脚本：{} ⇒ 无法做上传前复核。」
```

`IncidentTools::missing()`（`:181-192`）对 `bundle`、`triage`、`python` 三个路径逐个 `is_file()`。

**第二道闸**（即使脚本存在但没干活）：`preview_with`（`:244-270`）先看 `out.code != 0` ⇒ `Err("出包失败（脚本退出码 N）：stderr\nstdout")`；再看摘要能否读出 ⇒ `Err("脚本没有写出摘要 …：{io error}")`。⇒ **两条路都不可能返回 Ok，不存在「假装出包成功」**。

**真实 shell 复现**（脚本缺失时 App 会收到的东西）：

```
$ /bin/bash /tmp/definitely-not-here/incident-bundle.sh --out /tmp/probe.zip --json-out /tmp/probe.json
exit=127
stderr: /bin/bash: /tmp/definitely-not-here/incident-bundle.sh: No such file or directory
摘要是否被写出: no
$ /bin/bash -c true --out /tmp/probe2.zip --json-out /tmp/probe2.json   # exit 0 但什么都不写
exit=0   摘要存在: no
```

**覆盖缺口（§4 F-2）**：现有用例 `preview_fails_loudly_when_the_script_fails`（`:723`）用的是**替身返回 code 3 + stderr**，**没有**覆盖「脚本缺失（127 / spawn 失败）」与「exit 0 但没有摘要」这两条分支。生产行为我按上面的源码 + 真实 shell 判定为 fail-closed，但这两条分支**当前没有回归防线**。

---

## §3 对抗性变异探针（证明测试有牙，不是同义反复）

方法：`cp` 备份 → 精确锚点替换（断言锚点唯一，避免「空变异假绿」）→ 跑套件 → `trap` 还原。命令见 §6。

| 变异 | 改了什么 | 预期变红 | 实测 |
|---|---|---|---|
| **U1** | `useEffect` 里加 `void collect();`（挂载即自动出包） | 「未点击不出包/不上传」红线 | **8 条红**：`:95`、`:102`、`:136`、`:147`、`:161`、`:180`、`:188`、`:196` |
| **U2** | 剪贴板 `catch` 改成静默（删掉失败态） | 「复制失败必须可见」 | **恰好 2 条红**：`:206`、`:240` |
| **U3** | `kind==="secret"` 的文案从「已阻止上传」改为泛化「上传失败。」 | 反向验证「被拦住 ≠ 上传失败」 | **恰好 1 条红**：`:161` |

### 3.4 还原证据（原始输出）

```
mutation u1 applied (anchor x1)      → Test Files 1 failed (1); Tests 8 … （见上表）
mutation u2 applied (anchor x1)      → Tests 2 failed | 14 passed (16)
mutation u3 applied                  → Tests 1 failed | 15 passed (16)
restored_identical=yes   git_status_apps_ui=[]        # U1/U2 后
restored_identical=yes   git=[]                        # U3 后
```

还原后重跑最终态：UI `16 passed`、Rust `17 passed; 1 ignored`，`git status --porcelain` 为空（§1.2）。

---

## §4 发现（报 Lead 裁决；我只报不改）

| # | 级别 | 发现 | 建议 |
|---|---|---|---|
| **F-1** | 低（文档/配方） | `incident.rs:1062` 的手动验收注释给的 `XT_T130_BUNDLE=/tmp/xraytun-incident/…` 在 macOS 上**照抄必被拒**（`temp_dir()` 是 `/var/folders/…/T`；我按该配方跑就复现了拒绝）。这会让人误以为「上传功能坏了」 | 把注释里的路径改成 `"$(python3 -c 'import tempfile;print(tempfile.gettempdir())')/xraytun-incident/…"` 或 `$TMPDIR/xraytun-incident/…` |
| **F-2** | 中（回归防线缺口） |「脚本缺失（bash 127 / spawn 失败）」与「脚本 exit 0 但未写摘要」两条 fail-closed 分支**没有用例**（`:723` 只覆盖 code 3） | 加两条单测：`tools.bundle` 指向不存在路径；替身返回 `code 0` 且不写摘要 ⇒ 都必须 `Err` |
| **F-3** | 低-中（口径） | 哨兵角标只在挂载 + 上传成功后刷新，**不轮询/不订阅** ⇒ 页面开着时新异常不会即时 +1，与卡上「角标 +1」的字面读法有差 | 要么接受并在卡上写明「进入页面/上传后刷新」，要么加轻量轮询/事件 |
| **F-4** | 已声明边界 | 真机 WKWebView（剪贴板权限、窗口聚焦、真 Tauri IPC）、打包 `.app` 的 `resource_dir()` 解析、真机 helper 重装**未验**；本卡相关部分是组件级替代证据 | 留给真机手动验收；§2.1/§2.5 已列明替代覆盖不到的部分 |
| **F-5** | 运维知识（非缺陷） | 端点删除鉴权头是 **`X-Auth-Token`**；`Authorization: Bearer` 一律 401（且与「没令牌」同响应，不区分，符合不给穷举者信息的设计） | 我早前的 `ENDPOINT-PRIVACY-AUDIT.md` §1 记的是正确的 `X-Auth-Token`；本次先误用 Bearer 消耗了一个 401 探针，记录以免后人重复 |

**未发现**：静默失败、假承诺（「包已生成」而实际没有）、隐私泄漏（脏包未出门；manifest 无正文；错误只带位置/类型）。

---

## §5 结论

* **判定：ACCEPT（通过）**。六项验收里：
  * **真链验证**：item 2（一次真实上传，字节一致、manifest 无正文、删除复核 404、桶回 0）、item 3 的闸 A/B、item 4 的独立真相源；
  * **替代证据 + 有牙证明**：item 1、item 3 的界面口径、item 4 的组件红线（3 个变异探针）；
  * **源码级 fail-closed 判定 + 缺口标注**：item 5 的离线性、item 6 的两道闸（F-2 指出缺回归用例）。
* **预算与卫生**：POST **1/3**；端点对象数 **0 → 0**，无残留测试包；未改任何 `apps/**` 提交内容（变异全部还原并逐字节校验）；只写本文件。
* **建议后续卡**：F-2 的两条 fail-closed 回归用例（后端）、F-1 注释配方修正（顺带）、F-3 口径裁定（Lead）。

---

## §6 复现命令（逐条可粘贴）

```bash
# 0) 版本与卫生
cd /Users/xbtg-/deepseek-harness/xray-tun && git rev-parse HEAD && git status --porcelain
df -g /Users/xbtg- | tail -1                      # 余量 < 8~10 GiB 先停

# 1) 在哨兵允许的目录出包（macOS 必须用 temp_dir，不要用 /tmp）
T="$(python3 -c 'import tempfile;print(tempfile.gettempdir())')/xraytun-incident"; mkdir -p "$T"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
./scripts/incident-bundle.sh --out "$T/xraytun-incident-clean-$TS.zip"

# 2) 造脏包（含假凭据）并跑本地闸（应 3 命中 / 退出码 1）
rm -rf /tmp/stage-dirty && mkdir -p /tmp/stage-dirty && unzip -q "$T/xraytun-incident-clean-$TS.zip" -d /tmp/stage-dirty
printf 'vless://11111111-2222-3333-4444-555555555555@node.example.com:443?pbk=SECRETPBK&type=tcp#leak\n' > /tmp/stage-dirty/leak.txt
(cd /tmp/stage-dirty && zip -qr "$T/xraytun-incident-dirty-$TS.zip" .)
python3 scripts/triage-incident.py --privacy-check "$T/xraytun-incident-dirty-$TS.zip"; echo "GATE_EXIT=$?"

# 3) 组件级两条套件
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target \
  cargo test -p xraytun-desktop --lib -- incident
(cd apps/ui && npx vitest run src/incidentReport.test.tsx)

# 4) 脏包走 App 路径 ⇒ 必须 SecretDetected（且端点对象数仍为 0）
XT_T130_BUNDLE="$T/xraytun-incident-dirty-$TS.zip" \
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target \
  cargo test -p xraytun-desktop --lib real_upload_acceptance -- --ignored --nocapture

# 5) 干净包走 App 路径 ⇒ 1 次真实 POST（本卡预算内最后一次）
XT_T130_BUNDLE="$T/xraytun-incident-clean-$TS.zip" \
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target \
  cargo test -p xraytun-desktop --lib real_upload_acceptance -- --ignored --nocapture   # 记下 id

# 6) manifest（应无 ZIP 魔数/无包内容）+ 删除 + 复核 + 桶归零
ID=<上一步的 id>
curl -sS -D - -o /tmp/m.json "https://xraytun.top/api/incident/$ID"    # 200 / no-store / noindex
curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE "https://xraytun.top/api/incident/$ID"            # 401
curl -sS -o /dev/null -w '%{http_code}\n' -X DELETE -H "X-Auth-Token: $(cat ~/.xraytun-incident-endpoint-token)" \
     "https://xraytun.top/api/incident/$ID"                                                            # 200
curl -sS -o /dev/null -w '%{http_code}\n' "https://xraytun.top/api/incident/$ID"                       # 404
TOK="$(cat ~/.cf-incident-token)"; ACCT="$(curl -s -H "Authorization: Bearer $TOK" \
  https://api.cloudflare.com/client/v4/accounts | python3 -c 'import sys,json;print(json.load(sys.stdin)["result"][0]["id"])')"
curl -s -H "Authorization: Bearer $TOK" \
  "https://api.cloudflare.com/client/v4/accounts/$ACCT/r2/buckets/xraytun-incidents/objects" \
  | python3 -c 'import sys,json;print("count",len(json.load(sys.stdin).get("result",[])))'             # count 0

# 7) 变异探针（U1/U2/U3；一律 trap 还原 + 还原后校验）
#    模板见本报告 §3；还原后必须 shasum 相同且 git status --porcelain 为空
```

> 令牌只从文件读入、只经 header 传出：本报告与被执行命令都**不打印令牌值**。
