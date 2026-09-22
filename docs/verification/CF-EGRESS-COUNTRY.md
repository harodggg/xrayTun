# Cloudflare 能不能做「选国家/非香港」的出口？—— 实测结论：**不能**

> **为什么要写这份文档**：用户提出「加一个 CF 出口代理，让我能选香港→美国之类的国家」，后来收窄为「只要不是香港的就行」。
> 这是一个**看起来该有、实际不存在**的能力。文档的目的是让后来人不重复这次实验，
> 并且让「为什么最终选了别的路线」有可引用的依据。
>
> **采集方式**：只读 + 一个临时探测 Worker（`xraytun-egress-probe`，实验后已删除）。
> 采集点：用户机器（中国大陆）→ 经 XrayTun 隧道（香港节点 `45.207.197.185`）访问探测端点。

## 1. 结论先说

1. **普通 Worker 的出口由「接入的机房」决定**，用户在香港接入 ⇒ 出口在香港；没有参数可以选国家。
2. **Durable Object 的 `locationHint` 确实能把「执行机房」挪到别的区域**（实测 wnam→SJC、enam→EWR、weur→AMS、eeur→VIE、oc→SYD），
   **但目的地看到的出口 IP 完全没变**（所有区域都是同一个 `2a06:98c0:3600::103`，CF 的 anycast 地址，`cdn-cgi/trace` 报 `loc=HK`）。
3. ⇒ **从网站的角度，出口国仍然是香港。** CF 给不了「非香港」。
4. CF 唯一能「指定出口位置」的能力是 **Zero Trust 的 Egress policies + dedicated egress IPs**，
   官方文档第一行即标注 **Only available on Enterprise plans**，且作用于**跑 Cloudflare One Client 的设备流量**，
   不是任意 TUN 流量。

## 2. 原始测量

### 2.1 执行机房确实随 `locationHint` 变化

```
hint=oc    → http_egress_colo=SYD
hint=wnam  → http_egress_colo=SJC
hint=enam  → http_egress_colo=EWR
hint=weur  → http_egress_colo=AMS
hint=eeur  → http_egress_colo=VIE
hint=apac  → http_egress_colo=HKG
```

### 2.2 但**目的地看到的出口 IP 不变**（决定性）

第三方回显（`https://api.ipify.org?format=json`，在 DO 内部发起）：

| 位置 | 目的地看到的 IP |
|---|---|
| 普通 Worker（HKG） | `2a06:98c0:3600::103` |
| DO hint=wnam（SJC） | `2a06:98c0:3600::103` |
| DO hint=enam（EWR） | `2a06:98c0:3600::103` |
| DO hint=weur（AMS） | `2a06:98c0:3600::103` |
| DO hint=eeur（VIE） | `2a06:98c0:3600::103` |
| DO hint=oc（SYD） | `2a06:98c0:3600::103` |

并且 `cdn-cgi/trace` 对这个出口 IP 报 `loc=HK`。

### 2.3 `*.workers.dev` 在中国大陆直连不可达

```
经隧道：    HTTP 200，0.32 s
绕过隧道：  curl: (35) Connection reset by peer        ← 直连被 reset
```
⇒ 任何 CF 端点必须挂在自有域名的路径上（如 `xraytun.top/...`），不能依赖 `workers.dev`。

## 3. 「选国家」的正确做法（本项目的实际路线）

**出口国家由「真实位于该国的端点」决定，CF 只能做前置/隐藏/加速。**

1. **换/加一台非香港的机器**（免费额度或小 VPS），直接作为节点或**链式落地**（`香港节点 → 落地 → 目标`）；
2. 若该机器要做前置，用 **Cloudflare Tunnel / Argo** 隐藏源站 —— 出口国仍是那台机器所在国；
3. **只修网页流量**（HTTP(S)）的 Worker 方案不适用：Worker 选不了出口国（见 §2），而且它不是全流量。

## 4. 诚实清单（这次测量测不到什么）

* **只有一个采集点**（用户机器 + 香港节点）。「从别的地方接入会不会不同」没有测 —— 但 §2.2 的机制（同一 anycast 出口 IP）与接入点无关；
* **`connect()`（TCP sockets）那条路没有拿到独立回显**：探测里 `ipv4.icanhazip.com:80` 的响应体为空，
  所以「TCP 出口 IP 是否与 fetch 出口 IP 相同」**没有被直接测量**，本结论只由 fetch 侧的证据支撑；
* **没有测 Enterprise 的 Egress policies**（账号不是 Enterprise 套餐，无法验证其行为，只能引用官方文档）；
* 探测 Worker 已删除；本文档只保留测量结果与结论。
