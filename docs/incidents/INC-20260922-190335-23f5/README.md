# INC-20260922-190335-23f5 —— 一次**真实**的本机现场（只读采集）

> 这是本流程的**第一条真实记录**：在用户机器上（v0.8.34 正在运行、TUN 模式）跑了一次采集 + 分诊。
> 用途：① 证明流程端到端可用；② 给「分诊命中后怎么读」一个真实样例；③ 留下**原件指纹**。

## 1. 它是怎么产生的（可复现）

```bash
# 采集（2026-09-22 19:03:35 本地 / 11:03:35Z）
./scripts/incident-bundle.sh --out /tmp/inc-real2.zip --keep-dir
#   → 窗口被**退让**：ps 取不到 App 进程启动时刻（沙箱拒绝）⇒ 用「最近一次核心启动」
#   → core-tail.txt 截断：尾部 10416 行（窗口内共 56879 行）

# 分诊
python3 scripts/triage-incident.py --bundle /tmp/inc-real2.zip \
    --json-out incident.json --md-out SUMMARY.md
```

`INC-ID` 的 `<4hex>` = **manifest.json 的 sha256 前 4 位**（`23f5221f…`）⇒ ID 与内容绑定。

## 2. 分诊结果（口径见 `SUMMARY.md` 的口径头）

| 项 | 值 |
|---|---|
| signature | **`multiple`（命中 2 条）** |
| 命中 | **`loopback-hole`**：`route -n get 127.0.0.2` → **`interface: utun6`**（127/8 被指进隧道，不是 `lo0`） |
| 命中 | **`tun-iface-einval`**：`falied to set interface` **5 次 / 1.8 分钟 = 2.78 次/分**（阈值 1.0） |
| 未命中 | `v6-rewrite`(0 行)、`watchdog-false-positive`(0 次作废)、`probe-false-negative`(131/131 成功)、`log-read-loss`(0)、`log-write-interleave`(0 行多对象，App 0.8.34)、`helper-mismatch`(三态 Match) |
| 窗口 | 2026-09-22 18:46:04 → 19:08:50，**degraded = true**（退让口径，不是 App 启动至今） |
| 版本 | App **0.8.34** / 核心 **26.9.9** / helper 三态 **Match(0.8.34)** |

**读法**（这两条都在包里可复核）：
* `loopback-hole` 与 `docs/09-network-drop/LOOPBACK-ROUTE-BASELINE.md` 的 T3 是同一现象；
  这次现场是 **`utun6`**（TUN 正在接管），比「指向局域网网关」更严重 —— 回环流量被塞进隧道。
  ⇒ **后续时点**（2026-09-23 10:43 复采，仍然 `utun6`）与「何时才会消失」记在
  **`docs/incidents/OPEN-FINDINGS.md` 的 F-1**：这份记录是入口，那份是**跟踪**。
* `tun-iface-einval` 的原始行里还能看到 v0.8.34 的**新限流在起作用**
  （`source=app, level=warn`：`核心日志已限流：最近有 13 条未实时显示…`）—— 与本包 `events.jsonl` 一致。
  ⇒ 后续时点见 `OPEN-FINDINGS.md` 的 **F-2**（第二次采样 73.3 次/分，**窗口长度不同，不可直接比倍数**）。

## 3. 这一份里**没有**入库什么

* ❌ **完整 bundle**（`/tmp/inc-real2.zip`，约 2.5 MB + 展开后更长）：可能含隐私 ⇒ **不入库**。
  它的指纹留在 `evidence/sha256.txt`（`bundle_zip_sha256 = d148a7a2…`）。
* ❌ 完整 `core-tail.txt`（2.1 MB）与 `events.jsonl`（424 KB）⇒ 只留**判据用到的那几行**切片。
* ✅ 入库：`manifest.json`（原件副本）、`incident.json`、`SUMMARY.md`、两段最小证据切片、指纹。

> ⚠️ `manifest.json` 里保留了采集时的**本机绝对路径**（仓库里其它文档同样如此）。
> 如果要**把这份目录发给仓库外的人**，请先用同样的脱敏口径过一遍。

## 4. 这一份**不能**证明什么

1. `loopback-hole` / `tun-iface-einval` 是**症状**：本目录只记录「采集那一刻是这样」，
   不推断成因、也不判断是否由 XrayTun 造成（归属问题见 `task-80` 的现场取证口径）。
2. 窗口是**退让口径**（degraded）：不能当作「本次 App 启动至今」。
3. 采集是**时点快照**：同一条命令在别的时刻会得到不同的数字与指纹（这正是 `net-metrics.py` 强调口径头的原因）。
