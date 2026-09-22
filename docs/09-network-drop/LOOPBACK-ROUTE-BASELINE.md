# 回环路由：before / after 基线（真机只读采集，逐字留档）

> **为什么这份文件在仓库里**：这是 `63b84dc`（task-83「回环绝不经由物理网关」）的**真机对照基线**。
> 原始记录只写在 `/tmp`（`/tmp/diag-final.txt`），而 **`/tmp` 会被清** ——
> 本项目已经栽过一次（「不可复制的现场只存在于临时目录 = 等于不存在」），所以逐字收进仓库。
>
> **采集方式**：只读。`scripts/diagnose-network-drop.sh`（本轮的取证脚本）+ `netstat`/`route`/`ping`/`ifconfig`/`lsof`/`stat`。
> **没有**做任何有副作用的网络操作（`route add/delete`、改 DNS、改代理一律没做）。
> 机器：macOS 26.6.2（build 25G83）、内核 25.6.0、物理接口 `en0`、网关 `192.168.0.1`。

## 0. 两个时刻（这不是「一次故障」，是**两次不同状态**）

| | T1 =「bad route 在表里」 | T2 =「bad route 已消失，但回环**仍然**是坏的」 |
|---|---|---|
| 时间 | 2026-09-21 ~19:2x CST | **2026-09-22 11:19 CST** |
| 隧道 | xray 核心 **在**跑；`0/1 → utun6`；哨兵 DNS `198.18.0.2` | xray 核心 **不在**；无 `0/1`；无 utun 有地址；DNS `114.114.114.114` |
| `netstat` 的 127 行 | `127 → 192.168.0.1 UGSc en0` **在**（+ `127.0.0.1 → lo0`） | **只剩** `127.0.0.1 → 127.0.0.1 UH lo0` |
| `route -n get 127.0.0.2` | （当时未测 —— **这是我的采集缺口**） | `destination: default` · `gateway: 192.168.0.1` · `interface: **en0**` |
| `ping 127.0.0.2` | `sendto: Can't assign requested address` | **同样失败** |
| 取证脚本 §10 | 3 条（回环局部失效 / 回环路由形态异常 / utun 7 个可疑） | 2 条（**127.0.0.2 未走 lo0** / utun 6 个可疑） |

## 1. T1：`127` 被指向局域网路由器（bad route 在表里）

**原文**（`netstat -rn -f inet`，2026-09-21 19:1x）：

```
    0/1                utun6              UScg                utun6
    default            192.168.0.1        UGScg                 en0
    default            192.168.0.1        UGScIg                en0
    127                192.168.0.1        UGSc                  en0        ← ⚠️ 整个 127/8 被指向路由器
    127.0.0.1          127.0.0.1          UH                    lo0        ← 只有这个 /32 侥幸可用
    198.18.0.1         198.18.0.1         UH                  utun6
```

**可达性对照**（同一时刻，只读）：

```
  ping 127.0.0.1  → 通（64 bytes from 127.0.0.1: icmp_seq=0 ttl=64 time=0.067 ms）
  ping 127.0.0.2  → 不通：`ping: sendto: Can't assign requested address`
  nc -z 127.0.0.1:10808 → rc=0        nc -z 127.0.0.2:10808 → rc=1
  curl http://127.0.0.1:3080/ → http=401（DSH GUI 正常）
  curl http://127.0.0.2:3080/ → http=000     curl http://127.0.0.3:3080/ → http=000
  监听者：node 127.0.0.1:3080、xray 127.0.0.1:10808/10809（都只听 .1）
```

**归属（有代码证据，不是按进程名猜的）**：这条路由是 **XrayTun 自己装的** ——
修复前 `crates/xt-proto/src/lib.rs` 的 `default_bypass_networks()` **含 `"127.0.0.0/8"`**
（与连接态实读的旁路集合逐条吻合：`10/8、100.64/10、127/8、169.254/16、172.16/12、192.168/16、224/4`），
而 `crates/xt-tun/src/plan.rs` 把**所有 IPv4 旁路网段一律指向物理网关**；
`63b84dc`（task-83）已把 127/8 移出该列表，并在规划层加了「回环网段不装路由」的防御。
本机当时跑的是**已安装的 0.8.31**（`/Applications/XrayTun.app`，Info.plist = `0.8.31`），**在 `63b84dc` 之前**。

## 2. T2：bad route 不在了，**回环仍然被绕开** —— 这才是「after」该用的判据

2026-09-22 11:19 复采（隧道已断、xray 未跑、无 utun 地址）：

```
$ netstat -rn -f inet | grep -E '^127'
127.0.0.1          127.0.0.1          UH                    lo0
```

—— 显式的 `127 → 192.168.0.1` **已经不在了**。**但**：

```
$ route -n get 127.0.0.2
       route to: 127.0.0.2
    destination: default
        gateway: 192.168.0.1
      interface: en0                      ← ⚠️ 不是 lo0！
          flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>

$ route -n get 127.0.0.1
      interface: lo0                       （这个 /32 正常）

$ ping -c 1 127.0.0.2
ping: sendto: Can't assign requested address          ← **仍然失败**
```

⇒ **「坏路由消失」≠「回环恢复」。** T2 里 `127.0.0.2` 依然被绕开回环（落到 `default → 192.168.0.1 / en0`），
`ping` 依然失败。项目自己的注释（`plan.rs`）说「内核已经把 `127.0.0.0/8` 指向 `lo0`」——
**当前机器上不是这样**。

**⇒ 因此「after」的验收判据必须是路由层，而不是「netstat 里那条 127 行没了」**：

```
route -n get 127.0.0.2   # interface 必须是 lo0；落到 default/物理网卡 = 回环被绕开（异常）
ping -c 1 127.0.0.2      # 必须不再 "Can't assign requested address"
```

我已经把这条**决定性判据**加进 `scripts/diagnose-network-drop.sh` 的 §2c
（`nc` 只作旁证：它对「连接被拒绝」与「路由不通」都返回非 0，**不能单独定案**）。
注意 §3c（只看 `netstat` 里 `^127` 的形态）在 T2 会**看不到**这个异常 —— 两条是**互补**的，
只看 §3c 会漏、只看 §2c 少一点形态信息。

## 3. 期间机器上发生的**状态变化**（我两次读数不一致，如实记）

| 项 | 我 9/22 ~11:0x 读到 | 我 9/22 11:19 读到 | 说明 |
|---|---|---|---|
| 特权 helper | 「9月13 20:37、6,976,288 B」（我当时把路径写成了 `/Library/Application Support/com.xraytun.helper/`） | **`/Library/PrivilegedHelperTools/com.xraytun.helper`**、**mtime 2026-09-21 20:26**、**4,147,568 B**、sha256 `d0f2a26244525048bd1bcc10f1948d2d982bb6b1305ea0f7a93b8ccab107f30c` | **helper 已被重装**（App 包内 helper 的 sha **相同** ⇒ 两者现在 IDENTICAL） |
| LaunchDaemon plist | 「9月13 18:25」 | **2026-09-21 20:26**，`ProgramArguments` → `/Library/PrivilegedHelperTools/com.xraytun.helper run` | 同上，同一次安装 |
| 已安装 App | 未读版本 | **`0.8.31`**（Info.plist）、App 包 mtime `2026-09-21 18:28` | **还没换成 0.8.32** |
| `127 → 网关` 那条 | **在** | **不在** | 很可能是**断开连接时被 rollback 清掉**了 |

**我先前报告里的两处错，一并更正**（都是我自己的口径问题，不是机器的问题）：
1. helper 的路径我写成了 `/Library/Application Support/com.xraytun.helper/com.xraytun.helper`，
   **该路径不存在**；实际是 `/Library/PrivilegedHelperTools/com.xraytun.helper`。
2. 由此推的「已安装 helper 是 9月13 的」在 **11:19 这个时刻已经不成立** —— 现在它是 9/21 20:26 装的那份，
   且与 App 包内**逐字节相同**。**「谁在什么时候把它换成什么」我没有证据，只能记两个时刻的读数差。**

## 4. 这条基线怎么用（等用户重装/更新后）

1. **先复采对照条件**：确认还是同一台机器、同一网络（`en0`/网关 `192.168.0.1`）、隧道处于**同样的连接态**
   —— T1 是「连着」、T2 是「断开」，**两者不可直接比**；
2. 装上含 `63b84dc` 的版本（helper 侧要**重装助手**才生效）并**连上**；
3. 只读采 after：

```bash
route -n get 127.0.0.2                 # 期望：interface: lo0
netstat -rn -f inet | grep -E '^127'   # 期望：只有 127.0.0.1 → lo0，**没有** 127/8 指向网关
ping -c 1 127.0.0.2                    # 期望：不再 Can't assign requested address
nc -z 127.0.0.2 <一个只听 .1 的端口>     # 期望：连接被**拒绝**（说明能路由到 lo0），而不是「无路由」
bash scripts/diagnose-network-drop.sh --out /tmp/diag-after.txt   # §10 期望不再出现「127.0.0.2 未走 lo0」
```

## 5. T3（2026-09-22 13:43 CST）：**用户已升到 0.8.33、helper 也重装了，但回环仍然是坏的**

§4 的第 1、2 步**用户当天下午已经做完**，读数如下（全部只读）：

| 项 | T3 读数（2026-09-22 13:43） | 与 §3 的 T2 对比 |
|---|---|---|
| 已安装 App | **`0.8.33`**（`defaults read /Applications/XrayTun.app/Contents/Info.plist CFBundleShortVersionString`） | T2 是 `0.8.31` ⇒ **已升级** |
| 特权 helper | `/Library/PrivilegedHelperTools/com.xraytun.helper`、mtime **2026-09-22 13:17**、**4,180,656 B**、内嵌版本串 **`0.8.33`** | T2 是 9/21 20:26、4,147,568 B、0.8.31 ⇒ **已重装，且与 App 同版** |
| 隧道 | `0/1 → utun6`、`128.0/1 → utun6`、DNS `198.18.0.2`、**xray 在跑** | T2 是**断开**态 ⇒ **T3 是新的第三种状态：连着但回环仍坏** |

**关键证据（T2 没采到的那一格）** —— 在**隧道连接**状态下：

```
$ netstat -rn -f inet | grep -E '^(127|128|0/1|default)'
0/1                utun6              UScg                utun6
default            192.168.0.1        UGScg                 en0
default            192.168.0.1        UGScIg                en0
127.0.0.1          127.0.0.1          UH                    lo0        ← 只有 /32
128.0/1            utun6              USc                 utun6

$ route -n get 127.0.0.2
    destination: default
           mask: 128.0.0.0
      interface: utun6                     ← ⚠️ 被 0/1 吞进隧道（T2 是 en0，因为那时没隧道）

$ ifconfig lo0
    inet 127.0.0.1 netmask 0xff000000      ← ⚠️ 接口**仍然是 127/8**，但表里没有 127/8 那条

$ ping -c 2 127.0.0.2   → 2 packets transmitted, 0 received, 100.0% packet loss
$ curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3080/  → 401   （.1 走 /32，正常）
```

**T3 比 T2 多说明的两件事**：

1. **升级 + 重装 helper 不会修复这一条** —— 它是**已写进路由表的残留状态**，
   与 `63b84dc` 的代码修复无关（代码修复只保证「以后不再装那条路由」，已装出来的要清掉）。
   ⇒ **§4 第 2 步做完 ≠ after 通过**；必须**重启**（或 `sudo route add -net 127.0.0.0/8 -interface lo0`）才能回到干净基线。
2. **`ifconfig lo0` 的 netmask 仍是 `0xff000000`** ⇒ 「这台机器本来就没有 `127/8 → lo0` 表项」（§5 里的第二种可能）
   **被削弱**：接口层仍然声明自己覆盖 `127/8`，缺的是**路由表里那条连通路由**。
   （严格说这仍不是「干净机器对照」，但比 T2 时的证据强了一档，记在这里。）

**判据不变**（§4 第 3 步）：`route -n get 127.0.0.2` 的 `interface` 必须是 **`lo0`**；
`ping 127.0.0.2` 不再 100% 丢包。**这条以后由 tester 独立复核，判据以路由层为准。**

## 6. 诚实清单

* **T1 当时没有采 `route -n get 127.0.0.2`** —— 这是我采集上的缺口；T2 才补上，所以「T1 的路由出口是什么」
  严格说**没有直接观测**（我从 `netstat` 的 `127 → 192.168.0.1 UGSc en0` 行 + `ping` 失败推断，属**推断**）。
* **T2 的 `127.0.0.2 → default/en0` 是直接观测**（`route -n get`），但它**归因未定**：
  可能是 rollback 把显式路由删掉后没有恢复 `127/8 → lo0`，也可能是这台机器本来就没有那条表项
  （macOS 版本次差异）。**要定论需要一个「从未连过 XrayTun 的干净机器」作对照，我没有。**
* **helper 的两次读数差**：我只记录了「两个时刻各读到什么」，**没有**观测到那次重装动作本身，
  也没看到是谁触发的。
* 本文所有读数都是**只读**取得；**没有**对用户机器的网络配置做任何修改。
* `utun` 数量在 T1 是 7 个、T2 是 6 个 —— 我**不确定**差值来自哪条栈，按「仅提示可疑」记（§6b 的定位）。
