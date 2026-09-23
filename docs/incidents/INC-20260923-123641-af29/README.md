# INC-20260923-123641-af29 —— **第一份真实用户报告**（经 App 的「报告问题」上传）

> 这是「自动报错链」（`task-130`/`task-131`/`task-123`/`task-135`）上线后**第一次被真实使用**：
> 用户在本机 App 里点了「报告问题 → 确认上传」，包进了 R2，我按 `task-113` 的约定把它分诊入库。
> 对象仍在 R2（**30 天保留，不删** —— 那是用户的数据，属承诺保留期内）。
> 本目录只放**脱敏后的最小切片 + 指纹**；**完整 bundle 不入库**。

## 1. 怎么拿到的（可复现；令牌只本地用）

```bash
ID=INC-20260923-123641-af29
curl -H "X-Auth-Token: <token>" -o /tmp/inc170/bundle.zip \
     "https://xraytun.top/api/incident/$ID/blob"          # 128770 B
curl -s "https://xraytun.top/api/incident/$ID"            # public manifest
unzip -q /tmp/inc170/bundle.zip -d /tmp/inc170/x
python3 scripts/triage-incident.py --bundle /tmp/inc170/bundle.zip \
        --json-out incident.json --md-out triage.md
```

**我自己的独立核对（不是转述）**：

| 项 | 我量到的 | manifest 声明 | 结论 |
|---|---|---|---|
| zip sha256 | `505fb4e8e3da5a856e169e5398ec5c78e561dbe22a88c9a925874c23191fc266` | 同左 | **一致** |
| zip bytes | `128770` | `128770` | **一致** |
| 服务端 manifest | `received_at 2026-09-23T12:36:41.661Z`、`retention_days 30` | — | 与卡面一致 |
| 上传前隐私闸（App 跑的同一条） | `✓ 未发现疑似密钥模式`，**exit 0** | — | 用户这份包**过得了我们自己的闸** |

## 2. 本目录有什么

```
manifest.json      采集原件副本（**逐字节一致**，sha256 a89fe7e9…；注意：按约定它含本机绝对路径）
incident.json      triage 的机器读输出（原样，sha256 4ca4a821…）
SUMMARY.md         人读版：现象 / 证据 / 判定 / 结论与不确定项（我写的，含我自己的复核）
evidence/          脱敏后的最小切片 + 原件指纹（sha256.txt）
```

**没有入库**：完整 bundle（128,770 B）、`core-tail.txt`（2,097,388 B，**逐字节含 303 处节点地址/域名** ⇒ 必须脱敏）、
`events.jsonl` 全量（25 行，其中有 10 行 `connection was refused`）、`network.txt` 全量、`metrics.json` 全量。
原件指纹见 `evidence/sha256.txt`（含每个文件的 sha256 与字节数 ⇒ 可核对“原件长什么样”而不用把日志塞进仓库）。

## 3. 隐私

* **入库的每一份**都过了我自己的脱敏扫描：拿本机 `nodes.json` 当**独立真值**数「节点地址/域名」命中，
  再用正则抹 UUID/32 位 hex/分享链接/`/Users/<user>` ⇒ **切片与 incident.json/triage-raw.md 命中 0**。
* **例外（按约定）**：`manifest.json` 是**逐字节原件**，其中含 **1 处本机绝对路径**（`/Users/<user>/Library/…settings.json`）。
  `docs/incidents/README.md` §5 的约定就是「采集原件含本机绝对路径 —— 需要外发时先自己看一遍」；
  本目录的 `evidence/` 全部是脱敏后的，可以直接引用。
* 令牌一律写 `<token>`；本报告不含节点地址/UUID/口令。

## 4. 一条**与约定不符**的观察（交 Lead 判断）

`docs/incidents/README.md` §5 写：`INC-ID` 的 `<4hex>` = **manifest 的 sha256 前 4 位**（⇒ ID 与内容绑定）。
本体实测：ID 后缀 = `af29`，而这份 `manifest.json` 的 sha256 前 4 位 = `a89f` ⇒ **不相等**。
（上一份 `INC-20260922-190335-23f5` 是相符的：`23f5` ≡ 其 manifest sha256 前缀。）
⇒ 要么 App/服务端生成 ID 的方式变了，要么这条约定已经过时。**我只报事实，不改约定**。
