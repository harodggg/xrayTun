# `task-124`（诊断报告真脱敏）独立验证 —— **报告里还剩 2 行会泄漏节点地址**

> 作者：tester（**独立验证**，不采信实现者报告与单测）。**base = `cf9b8ad`**（delta 已并入）。
> ⚠️ **本报告不含任何真实节点地址 / UUID / 用户名原文**：命中项只写「类型 + 条数 + 行号 + **掩码窗口**」。

## 0. 口径

| 项 | 值 |
|---|---|
| 被验修订 | **`cf9b8ad`**（worktree 检出，独立 `CARGO_TARGET_DIR`：`.cargo-target.wt/v137b`） |
| 输入 | **本机真实日志全文** `~/Library/Application Support/com.xraytun.desktop/logs/app.jsonl`（运行中在被追加；见每次的数字） |
| 探针 | worktree 内新增一个只读测试，走**生产路径** `ReportRedaction::from_nodes_and_home(nodes, Some(HOME))` + `redact_secrets` |
| 输出 | 只打印**计数**；掩码明细写 `/tmp/v137b/leak-shape.txt`（值一律 `<NODE>`/`<USER>`） |
| 隔离与清理 | 突变只在 worktree 做；**收工立刻删掉 worktree 与它的 target dir**（磁盘 16→18 GiB） |

## 1. ⚠️ **明确回答：「真实日志里还能搜到节点地址吗？」→ 能，剩 2 行**

**独立真值**（来自 `nodes.json` 的 `address` + **节点名里形如 IP 的段**，**不**复用实现内部的地址列表）：

```
PROBE 行数=249024 节点=2 地址条目=3
PROBE 节点地址（**独立真值**）：原始命中行=37601 ⇒ 脱敏后=2      ← ❌ 还剩 2 行
PROBE 公网IP 1.0.0.1：原始=6290 ⇒ 脱敏后=0                     ← ✅
PROBE 用户名：原始=2 ⇒ 脱敏后=0                                 ← ✅（delta 生效）
PROBE 保留项：198.18.=51817 baidu=125 google=28825 127.0.0.=26881 192.168.=1884   ← ✅ 都还在
```
**掩盖不了的一点**：这 2 行是同一句模板，出现在两个时间点。掩码窗口（值已替换成 `<NODE>`）：
```
vec#28023 kind=ip 原始1次/脱敏后1次 before=hyphen after=other
  window=…rate_204 → 000；熄屏/换网/节点抖动，当前节点「Xray-<NODE>」），第 1 次自动重建开始"}…
```
⇒ **泄漏的是一个「节点 IP 嵌在节点显示名里」的形态**：`「Xray-<节点IP>」`。
* `before=hyphen` ⇒ `Xray-` 的 `-` 是**词字符**，边界判据 `boundary_before` 拒绝匹配；
* 而且该 IP 根本**没进判据集合**（2 个节点只产出 **3** 个条目 ⇒ 节点名里的 IP 没被 `hostname_runs` 取出来）
  —— 也就是说**两条路都漏**：既没进集合、进了也会被连字符边界挡下。
* **危害**：这 2 行正是「自愈事件」那一族（用户最可能贴出去的内容），而节点 IP 就是用户自己的服务器地址。
  ⇒ **报告贴到公开 issue 会泄漏服务器 IP**。这是本卡（P0·隐私）要防的那件事，**还没防住**。

### 1.1 其余泄漏搜索（都通过）
| 搜索项 | 原始 | 脱敏后 | 结论 |
|---|---|---|---|
| 节点地址/域名（独立真值） | 37,601 行 | **2 行** | ❌ 见上 |
| 公网 IP `1.0.0.1` | 6,290 行 | **0** | ✅ |
| `/Users/<用户名>`（含用户名子串） | 2 行 | **0** | ✅ delta 生效 |
| `198.18.x.x`（fake-IP 段） | —— | **51,817** 处 | ✅ 保留（delta 裁决 1 生效） |
| `www.baidu.com` / `google.com` | —— | 125 / 28,825 | ✅ 公开域名保留，报告仍可用 |
| `127.0.0.x` / `192.168.x` | —— | 26,881 / 1,884 | ✅ 本机管道保留 |
| UUID 形状 | —— | `<uuid>` 出现 508 次 | ✅ 已替换 |

## 2. 对抗性突变（**只在隔离 worktree**，4 组）

方法：**先把「探针版」存一份基准副本**，每次突变从副本还原再改实现（**不用 `git checkout`** —— 它会把探针一起还原）。

| 突变 | 改法 | 指定断言 | 结果 |
|---|---|---|---|
| **M1** 节点列表判据置空 | `build()` 首行把 `nodes` 影子成 `&[]` | `node` | **EXIT=101 红** ✅（独立真值仍看到 2 行） |
| **M2** 去掉公网 IP 分支 | `if contains(&ip) \|\| is_public_ip(ip)` → `if contains(&ip)` | `public` | **红** ✅ 断言原文「公网 IP 1.0.0.1 未被抹掉」 |
| **M3** `198.18.0.0/15` 挪到被抹侧 | `is_fake_ip_gateway = false` | `keep19818` | **红** ✅ |
| **M4** 关掉用户名抹除 | `home_mask` 用真实路径而不是 `masked_home` | `user` | **红** ✅ 断言原文「用户名仍出现在脱敏结果里」 |
| 还原（除 `node` 外全部断言） | 从基准副本还原 | `user,public,keep19818,keepnice` | **EXIT=0 绿** ✅ |

### 2.1 ⚠️ 我第一次跑 M1 得到的是**假绿**，必须记下来
第一版探针用 **实现内部的 `addresses.entries`** 去数「还有没有泄漏」。
M1 把判据集合**置空** ⇒ **探测器自己也跟着空了** ⇒ `red_hits = 0` ⇒ 断言通过 ⇒ **假绿**。
改成**独立真值**（直接读 `nodes.json`）之后，同一次突变立刻变红。原始对照：
```
（M1 下）PROBE 节点地址（**独立真值**）：原始命中行=37915 ⇒ 脱敏后=2
（M1 下）PROBE 对照：用**实现内部列表**去数 ⇒ 脱敏后=0   ← 用这个数就会假绿
```
⇒ **教训**：突变实验里，**判据不能与被测实现共用同一个真源**，否则「把真源清空」的突变会同时关掉探测器。

## 3. 「陈述 ↔ 实现」一致性（`apps/ui/src/pages/Logs.tsx`，`cf9b8ad`）

**delta 已经把 Lead 要求的两处点名写清** ✅：
```
「订阅 URL 是**只抹凭据、主机名（机场域名）会保留**」
「唯一覆盖不到的形态是 **base64 载荷**（如 vmess 分享链接里那段），日志里出现时请手动删掉再贴」
「用户主目录折成 /Users/<user>/…」　「本机管道地址保留（127/8、RFC1918、::1、ULA、198.18/15 fake-IP 网关段）」
```
**但有一句现在与实现不符**（就是我 §1 找到的那条）：

> 原文：**「只有一种形态覆盖不到」**（base64 载荷）

实际覆盖不到的**至少还有第二种**：**节点自己的 IP 出现在节点显示名里**（`「Xray-<节点IP>」`，被连字符边界与判据集合同时漏掉）。
⇒ **陈述比实现强**（说「只有一种」而实际有两种），**必须报 Lead**（我不改 UI）。
**建议改后措辞**（供参考，不代替实现者的判断）：
> 「覆盖不到的形态有两种：① **base64 载荷**（如 vmess 分享链接那段）；
> ② **节点 IP 出现在节点显示名里**（例如名字写成 `Xray-<IP>` 时）—— 贴出去前请自己在报告里搜一下节点地址。」

## 4. 实现者自称的既有结论核对

| 项 | 我量到的 | 说明 |
|---|---|---|
| `cargo test -p xraytun-desktop --lib -- diagnostics` | **20 passed / 2 ignored**（另有 **1 个失败 = 我加的探针**，即 §1 的发现） | 过滤串 `diagnostics`；两个 ignored 是网络类用例 |
| `cargo clippy -p xraytun-desktop --lib -- -D warnings` | **EXIT=0**（`Finished dev profile`） | ✅ 干净 |
| `type_contract` | **未跑**（按卡面：它预期红、等 `task-130`，不算本卡失败） | 诚实标注 |

## 5. 诚实清单（**这份验证覆盖不到什么**）

1. **base64 / vmess 分享链接里的地址**：实现不解析 base64 ⇒ 里面的节点地址**测不到、也不会被抹**；UI 已点名（这条是对的）。
2. **节点名里的 IP 形态**（§1）是**已实测**的漏项；但**同类还有多少**我没穷举
   （例如节点名写成 `HKG_45.207.197.185_01`、或名字里放域名却与 SNI 不同）—— 只证明了「存在」。
3. **SNI / transport host** 我没放进独立真值（怕类型猜错）⇒ 「域名形态的节点地址」我只覆盖了 `address` 与节点名里的 **IP**。
4. **其它 IP 段**（CGNAT `100.64/10`、文档段、组播…）按实现是「当作公网抹掉」；我**没有**逐段验证。
5. **UI 不可自动化**：我只**读**了 `Logs.tsx` 的文案（逐句对照），没有真机渲染截图。
6. **输入是时点快照**：日志在增长（三次运行分别是 200,449 / 230,207 / 249,024 行），所以**条数是时点值**；
   但「节点地址脱敏后还剩 2 行」在**三次运行里都成立**（不是抖动）。
7. **`Home` 判据只测了 `HOME` 环境变量那条路径**；若 `HOME` 取不到（`home = None`），实现会退化成「路径不脱敏」
   —— 那种情况下会怎样，我**没有构造**（生产路径应从快照传 `home`，属另一条线）。

## 6. 附录：探针源码与复现要点（`task-145` 直接复用）

```rust
// 加在 worktree 的 apps/desktop/src/commands/diagnostics.rs 的 `mod tests` 里（已随 worktree 删除）
#[test]
fn v137_real_log_redaction_probe() {
    use std::fs;
    let home = std::env::var("HOME").expect("HOME");
    let user = std::path::Path::new(&home).file_name().unwrap().to_string_lossy().to_string();
    let dir = std::path::PathBuf::from(&home).join("Library/Application Support/com.xraytun.desktop");
    let nodes: Vec<xt_core::model::Node> =
        serde_json::from_str(&fs::read_to_string(dir.join("nodes.json")).unwrap()).unwrap();
    // 走**生产路径**（`from_nodes` 是 #[cfg(test)] 的退路）
    let addresses = ReportRedaction::from_nodes_and_home(&nodes, Some(&home));
    let log = fs::read_to_string(dir.join("logs/app.jsonl")).unwrap();
    let all: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();

    // ⚠️ **独立真值**：只从 nodes.json 取（address + 节点名里形如 IP 的段），
    //    **绝不**复用 `addresses.entries` —— 否则「把判据置空」的突变会把探测器一起清空（实测假绿）。
    let mut truth: Vec<String> = nodes.iter().map(|n| n.address.clone()).collect();
    for n in &nodes {
        for tok in n.name.split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == ':')) {
            if tok.parse::<std::net::IpAddr>().is_ok() { truth.push(tok.to_string()); }
        }
    }
    truth.sort(); truth.dedup();
    let hit = |t: &str| truth.iter().any(|a| t.contains(a.as_str()));

    let redacted: Vec<String> = all.iter().map(|l| redact_secrets(l, &addresses)).collect();
    let raw_hits = all.iter().filter(|l| hit(l)).count();
    let red_hits = redacted.iter().filter(|l| hit(l)).count();
    let impl_hits = redacted.iter()
        .filter(|l| addresses.entries.iter().any(|a| l.contains(a.as_str()))).count();
    let red = redacted.join("\n");
    println!("PROBE 节点地址（独立真值）：原始={} ⇒ 脱敏后={}", raw_hits, red_hits);
    println!("PROBE 对照：用实现内部列表数 ⇒ {}", impl_hits);   // M1 下会是 0（假绿的来源）
    println!("PROBE 公网 1.0.0.1={} / 用户名={} / 198.18.={} / baidu={} / 127.0.0.={}",
        red.matches("1.0.0.1").count(), red.matches(&user).count(), red.matches("198.18.").count(),
        red.matches("www.baidu.com").count(), red.matches("127.0.0.").count());

    // 掩码诊断：把整行里所有 entry 换成 <NODE>，取命中点两侧各 36 字符 ⇒ 看「形状」而**不泄漏值**；
    // `before=/after=` 打印字符类别（`hyphen` 就是本次的根因）。
    // 断言按 `V137_ONLY=node,user,public,keep19818,keepnice` 门控，便于逐条突变。
    assert_eq!(red_hits, 0, "仍有节点地址/域名残留（不打印原值）");
}
```

**复现要点**：
```bash
df -h /                                   # 低于 10 GiB 停下报 Lead
./scripts/wt.sh new vXXX <冻结哈希>
# 把上面这段（含 V137_ONLY 门控与掩码诊断）注入该 worktree 的 diagnostics.rs
./scripts/wt.sh run vXXX -- cargo test -p xraytun-desktop --lib -- v137_real_log_redaction_probe --nocapture
./scripts/wt.sh rm vXXX                   # **收工立刻删**（含它自己的 target dir）
```
突变时**先把「探针版」`cp` 成基准副本**，每次从副本还原再改实现 —— **不要用 `git checkout`**：
它会把探针一起还原，之后 `-- <filter>` 匹配 0 个测试、退出码 0 ⇒ **假绿**（我踩过一次）。
