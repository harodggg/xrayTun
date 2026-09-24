//! Xray 配置生成：把领域模型渲染成 Xray-core 能吃的 JSON。
//!
//! 生成的配置遵循以下固定骨架（顺序即含义，不要随意调整）：
//!
//! ```text
//! log      日志级别与输出
//! api      管理 API（本机 10085）
//! dns      内核 DNS 模块（可选 fakedns）
//! fakedns  Fake-IP 地址池（可选）
//! inbounds socks / http / tun
//! outbounds node-* / direct / block / dns-out / api
//! routing  DNS 劫持 → API → 用户规则 → 兜底
//! policy   统计开关
//! ```
//!
//! 四条容易被忽略但很关键的细节：
//!
//! 1. **DNS 劫持规则必须排在用户规则最前面**。否则用户写的 `port: "53" → direct`
//!    或者一条 catch-all 会先命中，DNS 就再也到不了 `dns-out`。
//! 2. **SOCKS 入站必须开 `udp: true`**。TUN 模式下的 UDP / QUIC / DNS 中继
//!    依赖 SOCKS5 的 UDP ASSOCIATE；不开就等于没有 UDP。
//! 3. **Xray 有原生 TUN 入站**（`"protocol": "tun"`，内置 gVisor 协议栈），
//!    macOS 支持自 **v26.1.18** 起。所以本工具不再需要 tun2socks 旁路进程 ——
//!    但这也意味着**核心版本必须是新的**，旧版本要退回 `ExternalTun2Socks` 路径。
//! 4. **Xray 也有原生 Fake-IP**（`fakedns` + `destOverride: ["fakedns+others"]`），
//!    并非 sing-box 独有。默认关闭，因为它会污染本机 DNS 缓存。

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Map, Value};

use crate::model::{AppSettings, DnsHandling, Node, Protocol, RoutingPreset, Transport};
use crate::routing::{self, RoutingRule};

/// 管理 API 的本地端口。
///
/// 注意：**这不是 Xray 的默认端口**（Xray 没有默认端口，`listen` 缺省为空）。
/// 10085 只是官方文档和社区示例里通用的约定值，我们沿用它是为了让
/// 用户拿官方文档排障时不会困惑。
pub const API_PORT: u16 = 10085;

/// Xray 原生 TUN 入站在 macOS 上**完整可用**（建卡 + 配地址 + 装路由 +
/// 支持 `XRAY_TUN_FD`）的最低核心版本。
///
/// 演进过程（已核对上游源码）：
///
/// | 版本 | macOS 上的能力 |
/// |---|---|
/// | < v26.1.18 | 没有 `proxy/tun`，`protocol: "tun"` 直接启动失败 |
/// | v26.1.18 | 有 `tun_darwin.go`，但只建卡 + 设 MTU，**不配地址不装路由** |
/// | **v26.1.31** | 补上地址/路由编程，并开始支持 `XRAY_TUN_FD` |
///
/// 所以我们把下限钉在 v26.1.31；实践上建议 >= v26.9.x。
pub const MIN_CORE_VERSION_NATIVE_TUN: &str = "26.1.31";

/// Xray 原生 `tun` 入站的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunInboundSpec {
    /// `None` 表示让内核自己挑一个 `utunN`（Xray 默认在 10..1024 之间随机）。
    pub name: Option<String>,
    pub mtu: u16,
    /// 隧道内网关地址，形如 `198.18.0.1/15`。
    pub gateway: String,
    /// 交给 TUN 入站的 DNS 列表。留空则不填该字段。
    pub dns: Vec<String>,
    /// 由 Xray 自动安装的系统路由。
    ///
    /// 我们用 `0.0.0.0/1` + `128.0.0.0/1` 而不是 `default`：不破坏原默认路由，
    /// 回滚只需删除这两条。
    pub auto_system_routing_table: Vec<String>,
    /// 出站 socket 绑定的物理接口。
    ///
    /// 这是**比手工加 host 路由更干净的防环手段**：把出站绑定到 `en0` 之后，
    /// 「连代理服务器」的流量根本不经过隧道，不存在路由环。
    /// 在 macOS 上 Xray 用 `IP_BOUND_IF` 实现。
    pub auto_outbounds_interface: Option<String>,
}

/// 入站配置档位。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundProfile {
    /// 只做本机 SOCKS/HTTP 代理（「系统代理」模式）。
    LocalProxy,
    /// TUN 模式：走 Xray 原生 tun 入站。
    Tun(TunInboundSpec),
    /// TUN 模式，但数据面交给外部 tun2socks（旧版核心的退路）。
    TunExternalDatapath,
}

impl InboundProfile {
    pub fn is_tun(&self) -> bool {
        matches!(self, Self::Tun(_) | Self::TunExternalDatapath)
    }
}

/// 根据设置推导出原生 TUN 入站的参数。
pub fn tun_inbound_spec(
    settings: &AppSettings,
    physical_interface: Option<&str>,
    auto_routes: bool,
) -> TunInboundSpec {
    let tun = &settings.tun;
    TunInboundSpec {
        name: None, // 让内核挑，避免与用户已有的 utun 冲突
        mtu: tun.mtu,
        gateway: tun.network.clone(),
        dns: if tun.sentinel_dns.is_empty() { Vec::new() } else { vec![tun.sentinel_dns.clone()] },
        auto_system_routing_table: if auto_routes {
            let mut v = vec!["0.0.0.0/1".to_string(), "128.0.0.0/1".to_string()];
            if tun.ipv6 == xt_proto::Ipv6Mode::Override {
                v.push("::/1".into());
                v.push("8000::/1".into());
            }
            v
        } else {
            Vec::new()
        },
        auto_outbounds_interface: tun
            .bind_outbound_to
            .clone()
            .or_else(|| physical_interface.map(|s| s.to_string())),
    }
}

pub struct CoreConfigInput<'a> {
    pub settings: &'a AppSettings,
    pub nodes: &'a [Node],
    /// 当前选中的节点 id。`None` 时所有流量走 `direct`。
    pub selected: Option<&'a str>,
    /// 额外规则（预设 + 自定义已由调用方合并好）。
    pub rules: &'a [RoutingRule],
    pub profile: InboundProfile,
    /// 物理网卡名（如 `en0`）。
    ///
    /// TUN 模式下必须提供：`direct` 出站要把 socket 绑到它，否则直连流量的
    /// 包会被 `0.0.0.0/1` 送回隧道，形成路由环。
    pub physical_interface: Option<&'a str>,
}

/// 生成完整的 Xray 配置。
pub fn build(input: &CoreConfigInput<'_>) -> Value {
    let s = input.settings;
    let selected_tag = input
        .selected
        .map(|id| format!("node-{id}"))
        .unwrap_or_else(|| "direct".to_string());

    let mut root = Map::new();
    root.insert("log".into(), build_log(s));
    root.insert("api".into(), build_api());
    root.insert("dns".into(), build_dns(s));
    if let Some(fd) = build_fakedns(s) {
        root.insert("fakedns".into(), fd);
    }
    root.insert("inbounds".into(), build_inbounds(s, &input.profile));
    root.insert("outbounds".into(), build_outbounds(input.nodes, input));
    root.insert(
        "routing".into(),
        build_routing(input.rules, &selected_tag, sentinel_dns_of(s, &input.profile)),
    );
    root.insert("policy".into(), build_policy());
    root.insert("stats".into(), json!({}));

    Value::Object(root)
}

/// 序列化为带缩进的 JSON 字符串，便于落盘后人工排查。
pub fn build_pretty(input: &CoreConfigInput<'_>) -> String {
    serde_json::to_string_pretty(&build(input)).expect("生成的配置一定可序列化")
}

// ---------------------------------------------------------------------------
// 各段落
// ---------------------------------------------------------------------------

fn build_log(s: &AppSettings) -> Value {
    // 空字符串表示输出到 stdout/stderr，由我们捕获后转发给 UI。
    json!({
        "loglevel": s.log_level,
        "access": "",
        "error": "",
        "dnsLog": false,
        "maskAddress": ""
    })
}

fn build_api() -> Value {
    json!({
        "tag": "api",
        "services": ["HandlerService", "LoggerService", "StatsService", "RoutingService"]
    })
}

/// Fake-IP 地址池。未启用时返回 `None`（配置里不出现该字段）。
fn build_fakedns(s: &AppSettings) -> Option<Value> {
    if !s.fakedns.enabled {
        return None;
    }
    Some(json!({
        "ipPool": s.fakedns.ip_pool,
        "poolSize": s.fakedns.pool_size
    }))
}

fn build_dns(s: &AppSettings) -> Value {
    let d = &s.dns;

    // hosts：静态映射。Xray 的 key 支持 `domain:` / `full:` / `regexp:` 前缀，
    // 用户填的是普通域名，这里统一加上 `domain:` 让它匹配子域。
    let mut hosts = Map::new();
    for (domain, value) in &d.hosts {
        let key = if domain.contains(':') { domain.clone() } else { format!("domain:{domain}") };
        hosts.insert(key, Value::String(value.clone()));
    }

    let servers: Vec<Value> = match d.mode {
        DnsHandling::Direct => d
            .direct_servers
            .iter()
            .map(|a| Value::String(a.clone()))
            .collect(),
        DnsHandling::Proxy => d
            .remote_servers
            .iter()
            .map(|a| Value::String(a.clone()))
            .collect(),
        DnsHandling::SplitByRule => {
            // 关键点：用 `domains` 让内核按域名选解析器。
            // 大陆域名用国内 DNS 直连解析（拿到就近 CDN IP），
            // 其余域名走远端加密 DNS（抗污染，且解析结果与出口位置一致）。
            // **刻意不写 `expectIPs`。**
            //
            // 最初这里给两个解析器都加了 `expectIPs`（大陆域名要求返回
            // `geoip:cn` 的地址，反之亦然），本意是挡掉明显不合理的答案。
            // 实测它造成的伤害远大于收益：
            //
            //     UDP:223.5.5.5:53 got answer: api.deepseek.com TypeA -> [3.173.21.63], rtt: 1.19ms
            //     failed to lookup ip for domain api.deepseek.com at server UDP:223.5.5.5:53
            //       > features/dns: empty response
            //     DOH//1.1.1.1 querying: api.deepseek.com.
            //
            // 国内的解析器 **1.2ms 就给出了正确答案**，只因为
            // `3.173.21.63`（AWS）不在 `geoip:cn` 里就被丢弃，然后串行回退到
            // DoH —— 一次查询从 1ms 变成 450ms 起步。而「国内域名解析到
            // 海外 IP」是常态：Apple、DeepSeek 这类都在用海外云。
            //
            // 回退链一长，再叠上节点抖动，查询就会超过内核的 DNS 超时，
            // 日志里刷 `context canceled`，用户看到的是「什么都打不开」。
            //
            // 去掉之后：谁被 `domains` 选中就用谁的答案，不再事后否定它。
            // **同一条 `domains` 下的候选要全部写进去** —— 那才是同层回退链。
            //
            // 之前对每个列表只取第一个（`first_or`），后果在真实日志里露出来了：
            // 国外 DoH 超时之后，回退链上**再没有别的国外解析器**了，于是被墙
            // 域名被国内解析器接着答出来（污染 / 错误 IP）：
            //
            //     failed to retrieve response for alive.github.com.
            //       > Post "https://1.0.0.1/dns-query": context deadline exceeded
            //
            // 隔离实例实测（把一个国外候选指向必然超时的 192.0.2.1）：
            //
            //   只取第一个     第二台被查的是 UDP:223.5.5.5:53（国内）
            //   全部写进去     第二台被查的是 DOH//1.1.1.1（仍是国外）
            //
            // 这也让「自动选优」的排序真正有意义：第 2..n 位不再是白排的，
            // 而是回退时真会用到的备选。
            let remote = servers_or_default(&d.remote_servers, "https://1.1.1.1/dns-query");
            let direct = servers_or_default(&d.direct_servers, "223.5.5.5");

            let mut out = Vec::new();
            for addr in &remote {
                out.push(json!({ "address": addr, "domains": ["geosite:geolocation-!cn"] }));
            }
            for addr in &direct {
                out.push(json!({ "address": addr, "domains": ["geosite:cn"] }));
            }
            // 兜底：一条 `domains` 都没匹配上的域名。这里保持原来的语义，只放
            // 每组的**第一台** —— 走到这一步说明前面没命中，再列一遍同层候选
            // 没有额外意义（Xray 自己的 fallback 会把它们排在后面）。
            out.push(json!({ "address": first_or(&d.direct_servers, "223.5.5.5") }));
            out.push(json!({ "address": first_or(&d.remote_servers, "https://1.1.1.1/dns-query") }));
            out
        }
        DnsHandling::Custom => d
            .remote_servers
            .iter()
            .chain(d.direct_servers.iter())
            .map(|a| Value::String(a.clone()))
            .collect(),
    };

    // **刻意不加 `localhost` 兜底。**
    //
    // `localhost` 在 Xray 里表示「用操作系统的解析器」。而 TUN 模式下我们把
    // 系统 DNS 指向了隧道内的哨兵地址（198.18.0.2）—— 于是这个兜底会变成：
    //
    //     内核 DNS 模块 → 系统解析器 → 哨兵地址 → 进隧道 → 又回到内核 DNS 模块
    //
    // 一个自指的死循环。实测症状：
    //     lookup xxx on 198.18.0.2:53: dial udp 198.18.0.2:53: connect: network is unreachable
    //
    // 去掉它没有损失：上面已经有两组显式解析器（远端 DoH + 直连 IP），
    // 它们各自都是完整可用的，不需要再兜一层必然会绕回自己的东西。
    let mut servers = servers;

    // Fake-IP 必须在最前面：它是「立即返回假地址」的解析器，
    // 排在后面就永远轮不到它，域名分流也就失效了。
    if s.fakedns.enabled {
        servers.insert(0, json!({ "address": "fakedns" }));
    }

    json!({
        "hosts": Value::Object(hosts),
        "servers": servers,
        "queryStrategy": d.query_strategy,
        "disableCache": d.disable_cache,
        "disableFallback": false,
        "tag": "dns-module"
    })
}

fn first_or(list: &[String], fallback: &str) -> String {
    list.first().cloned().unwrap_or_else(|| fallback.to_string())
}

/// 列表为空时退回一个硬编码默认值（用户可能把列表清空）。
///
/// 非空时**原样返回整个列表** —— 不要在这里只取第一个：同一条 `domains` 下的
/// 候选构成回退链，砍掉后面的会让「国外解析器挂了」直接回退到国内解析器
/// （见 [`build_dns`] 里 `SplitByRule` 的说明）。
fn servers_or_default(list: &[String], fallback: &str) -> Vec<String> {
    if list.is_empty() {
        vec![fallback.to_string()]
    } else {
        list.to_vec()
    }
}

fn build_inbounds(s: &AppSettings, profile: &InboundProfile) -> Value {
    // `fakedns+others` 的含义：先尝试把假地址还原成域名；不是假地址时
    // 再用常规的 SNI / Host 嗅探。这正是 sing-box 里 fakeip + sniffing 的组合语义。
    let dest_override: Vec<&str> = if s.fakedns.enabled {
        vec!["fakedns+others"]
    } else {
        vec!["http", "tls", "quic"]
    };
    let sniffing = json!({
        "enabled": s.dns.sniffing || s.fakedns.enabled,
        "destOverride": dest_override,
        // routeOnly=false：允许内核用嗅探出的域名重新解析并据此分流。
        // 打开 Fake-IP 时必须保持 false，否则假地址不会被还原。
        "routeOnly": false
    });

    let listen = if s.allow_lan { "0.0.0.0" } else { "127.0.0.1" };

    let mut inbounds = vec![
        json!({
            "tag": "socks",
            "listen": listen,
            "port": s.socks_port,
            "protocol": "socks",
            "settings": {
                "auth": "noauth",
                // 保留 UDP：外部数据面（tun2socks）路径完全依赖它。
                "udp": true,
                "userLevel": 0
            },
            "sniffing": sniffing
        }),
        json!({
            "tag": "http",
            "listen": listen,
            "port": s.http_port,
            "protocol": "http",
            "settings": { "userLevel": 0 },
            "sniffing": sniffing
        }),
    ];

    // API 入站：只监听回环。
    inbounds.push(json!({
        "tag": "api",
        "listen": "127.0.0.1",
        "port": API_PORT,
        "protocol": "dokodemo-door",
        "settings": { "address": "127.0.0.1" }
    }));

    // Xray 原生 TUN 入站。
    if let InboundProfile::Tun(spec) = profile {
        let mut settings = serde_json::Map::new();
        if let Some(name) = &spec.name {
            settings.insert("name".into(), Value::String(name.clone()));
        }
        settings.insert("mtu".into(), json!(spec.mtu));
        settings.insert("gateway".into(), json!([spec.gateway.clone()]));
        settings.insert("userLevel".into(), json!(0));
        if !spec.dns.is_empty() {
            settings.insert("dns".into(), json!(spec.dns.clone()));
        }
        if !spec.auto_system_routing_table.is_empty() {
            settings.insert(
                "autoSystemRoutingTable".into(),
                json!(spec.auto_system_routing_table.clone()),
            );
        }
        if let Some(iface) = &spec.auto_outbounds_interface {
            settings.insert("autoOutboundsInterface".into(), Value::String(iface.clone()));
        }

        inbounds.push(json!({
            "tag": "tun",
            "protocol": "tun",
            "settings": Value::Object(settings),
            "sniffing": sniffing
        }));
    }

    Value::Array(inbounds)
}

fn build_outbounds(nodes: &[Node], input: &CoreConfigInput<'_>) -> Value {
    let mut out = Vec::with_capacity(nodes.len() + 4);
    for node in nodes {
        out.push(node_to_outbound(node));
    }

    // 直连出站：把「未选中节点」或直连规则落到这里。
    //
    // `domainStrategy` 的位置在 Xray 26.x 变过：原来在 `settings` 里，
    // 现在迁到了 `streamSettings.sockopt`。旧写法仍会被自动迁移，
    // 但核心每次启动都会打一条 deprecation 警告，且上游明确说了
    // "will be removed" —— 所以直接用新写法。
    //
    // # 为什么这里**必须**显式绑接口
    //
    // TUN 模式下 `0.0.0.0/1` 指向 utun，直连出站的包会被默认路由送回隧道，
    // 形成路由环。Xray 的 tun 入站本来会通过 `autoOutboundsInterface`
    // 注册一个**全局** dialer controller 来做这件事，但实测它没有作用到
    // `freedom` 的 socket 上（直连目标报 `network is unreachable`，
    // 而同一时刻绑定了 en0 的普通 socket 一切正常）。
    //
    // 所以这里逐出站显式写 `sockopt.interface`。这条路走的是
    // `applyOutboundSocketOptions`，不依赖任何全局状态，是确定性的。
    //
    // # `autoOutboundsInterface` 我们**保留**，不删（task-93 的结论）
    //
    // 上面那个全局 controller 在核心 26.9.9 上会刷同一条日志 —— 实测本机
    // `app.jsonl` 里 **673 次**：
    //
    //     proxy/tun: [tun] falied to set interface > invalid argument
    //
    // 机制已经用受控实验定下来了（见 `crate::net::bind_to_interface_fd` 的文档
    // 和它的测试）：`IPPROTO_IP` + `IP_BOUND_IF` **只对 AF_INET socket 有效**，
    // 喂给 AF_INET6 socket 在**任何状态下**都是 `EINVAL`；v6 要用
    // `IPPROTO_IPV6` + `IPV6_BOUND_IF`。日志自己就把族露出来了：
    //
    //     dialing to udp:218.30.118.6:53
    //       → proxy/tun: [tun] falied to set interface > invalid argument
    //     proxy/freedom: connection opened to udp:218.30.118.6:53,
    //                    local endpoint [::]:55819, remote endpoint 218.30.118.6:53
    //
    // 目标是 v4 字面量，**local endpoint 却是 `[::]`** —— Go 建的是 dual-stack
    // socket，v4 选项必然失败。EINVAL 的目标里多数本来就是 IPv6 目标
    // （`tcp:[240e:…]:80`，国内电信 v6）。所以核心缺的是这条**按族分支**。
    //
    // 由此三个判断：
    //
    // 1. 这层兜底在**这些** socket 上没生效（错误被 LogInfo 吞掉，dial 继续）。
    //    这是核心的缺陷，配置改不了 socket 的地址族，`sockopt` 里也没有这种开关。
    // 2. 但**删掉 `autoOutboundsInterface` 不是治病**：日志会消失，那些 socket
    //    照样是没绑上的状态；而它还是**自己没有 `sockopt.interface` 的那些 dialer**
    //    （节点出站就是 `sockopt: null`）唯一的兜底 —— 删了只会更差。
    //    「错误没了」不等于「病好了」。
    // 3. 我们**不靠它**：每个出站的 `sockopt.interface` 才是确定性那条路。DNS
    //    上游的查询经 dispatcher 落到**承载它的出站**（国内明文 DNS 就是
    //    `direct`，见 `build_routing` 的 `preset-cn-ip` 注释），绑网卡由那个出站
    //    负责；走节点的 DoH 靠 xt-tun 给代理服务器装的 host 路由直接出物理口。
    //
    // 结论：保留它，当作「已知缺陷 + 兜底」，升级捆绑核心时复查这条日志是否
    // 消失（等上游补上 v6 分支），并考虑向上游报。**未验证项**：`direct` 出站
    // 自己的 `sockopt.interface` 在 `[::]` 双栈 socket 上是否真的绑上了 ——
    // 日志里没有第二条绑卡错误，但我们没有直接证据（见 task-93 报告）。
    // # `domainStrategy` 为什么是 `UseIPv4` 而不是 `UseIP`（task-97，实测）
    //
    // `UseIP` = 让核心**自己解析域名并拨解析出来的 IP**，于是 AAAA 也会被用上。
    // 而这台机器根本没有可用的 IPv6（`ifconfig en0` 只有 `fe80::…%en0`；
    // `route -n get -inet6 default` = not in table）。实测（本机 `app.jsonl`
    // 2026-09-22 全天 123,541 行，按 session id 归并连接）：
    //
    //     只有 v6 目标改写的连接 42 条 → **40 条**随后 `proxy/freedom:
    //       failed to open connection`（95.2%）
    //     只有 v4 目标改写的连接 408 条 → **0 条**失败（0%）
    //     `replace destination with tcp:[240e:` 534 次；47 条连接失败里
    //     40 条是纯 v6 dial（85%）
    //
    // 典型轨迹（同一连接）：客户端给的是 **IPv4** 目标 `tcp:183.2.172.177:443`
    // → sniff 出 `www.baidu.com` → `[preset-cn-domain] → direct` → 核心解析出
    // AAAA → `replace destination with tcp:[240e:…]:443` 连试 5 次 → 全失败 →
    // 放弃。同域名走 `:80` 那次抽到 v4 就成功 —— 这正是用户说的
    // 「国外可以、国内时不时直接断掉」。
    //
    // 选 `UseIPv4` 而不是 DNS 层 `queryStrategy: "UseIPv4"`：后者会改**内建 DNS
    // 模块的答案**（客户端要的 AAAA 一起吞掉、并波及境外解析），比缺陷本身大；
    // 这里只收敛 `direct` 这一个 dialer。也不用 `AsIs`：官方文档写明 AsIs 走 Go 的
    // Happy Eyeballs，**TCP 仍然优先 IPv6**，等于没修（`https://xtls.github.io/config/transports/sockopt.html`）。
    //
    // 代价（两条修法共同）：**双栈用户对国内站点也走 v4**，不再享受 v6。
    // `UseIPv4` 在域名只有 AAAA、没有任何 A 时会按官方文档回退到 `AsIs`（不是硬失败）。
    // 境外/代理路径不受影响：节点出站的 `sockopt` 仍是 `null`，目标域名交给节点侧解析。
    let mut direct_sockopt = serde_json::Map::new();
    direct_sockopt.insert("domainStrategy".into(), "UseIPv4".into());
    if let Some(iface) = input.physical_interface {
        direct_sockopt.insert("interface".into(), Value::String(iface.to_string()));
    }
    out.push(json!({
        "tag": "direct",
        "protocol": "freedom",
        "settings": {},
        "streamSettings": { "sockopt": Value::Object(direct_sockopt) }
    }));

    // 拦截出站。
    out.push(json!({
        "tag": "block",
        "protocol": "blackhole",
        "settings": { "response": { "type": "http" } }
    }));

    // DNS 出站：被路由到这里的 DNS 查询交给内核 DNS 模块，它只答 A/AAAA。
    //
    // **刻意不给它加 `settings`。** 非 A/AAAA 的查询（PTR / SVCB / HTTPS RR）
    // 因此走内核默认行为：立刻回一个**空 NOERROR**。隔离实例实测，TYPE65
    // 查询 1ms 返回 `ANSWER: 0`，客户端随即回退去问 A 记录 —— 这是正常且最快的。
    //
    // 试过两条"更漂亮"的路，都不划算（实测见 docs/04 §6.8）：
    //
    //   nonIPQuery: "drop"  → 直接不回包，客户端一路等到超时；而且这个字段
    //                         已被内核标为 deprecated，启动时会打警告。
    //   rules + direct      → 把 1ms 的本地空应答换成一次真实上游往返，
    //                         却仍然拿不到 HTTPS RR（223.5.5.5 也不提供）。
    //
    // 也就是说：日志里那条 `proxy/dns: rejected type ... query` 不是故障，
    // 别再去"修"它。它是 [Info] 级 —— 之前它出现在「错误」页签，是因为我们
    // 自己的日志分类器按关键字判级（见 apps/desktop/src/commands.rs）。
    out.push(json!({ "tag": "dns-out", "protocol": "dns" }));

    // API 出站：与 `api.tag` 同名，是内核内部约定的管理通道。
    out.push(json!({ "tag": "api", "protocol": "freedom", "settings": {} }));

    Value::Array(out)
}

/// TUN 档位下写入系统的那台「哨兵 DNS」。非 TUN 档位返回 `None`。
fn sentinel_dns_of<'a>(s: &'a AppSettings, profile: &InboundProfile) -> Option<&'a str> {
    if !matches!(profile, InboundProfile::Tun(_) | InboundProfile::TunExternalDatapath) {
        return None;
    }
    let sentinel = s.tun.sentinel_dns.trim();
    (!sentinel.is_empty()).then_some(sentinel)
}

/// App 在 `routing.rules` 里**自己追加**的三条内部规则 tag（不来自用户设置）。
///
/// 它们是**保留名**：用户的自定义规则若取了同名 id，最终配置里就会出现重复
/// `ruleTag`，而 Xray 在 `app/router` 阶段**直接拒绝启动** —— 与「预设 × 自定义」
/// 撞名是同一类故障（tester 实测：三个值各让真实核心输出
/// `duplicate ruleTag internal-*` 并且 exit 23）。
/// 兜底见 [`uniquify_rule_tag_values`]（它在**最终 rules 数组**上做唯一化）。
const RULE_TAG_DNS_HIJACK: &str = "internal-dns-hijack";
const RULE_TAG_API: &str = "internal-api";
const RULE_TAG_FALLBACK: &str = "internal-fallback";
const INTERNAL_RULE_TAGS: [&str; 3] = [RULE_TAG_DNS_HIJACK, RULE_TAG_API, RULE_TAG_FALLBACK];

/// 自检失败时附给用户看的核心原始输出**末尾行数**。
///
/// 实测（本次 P0）：核心失败时 stderr = 0 字节、stdout = 3445 字节 / 35 行
/// （首行是版本横幅、10 行 `[Debug]`，**可操作的那句在最后一行**）。
/// 原样透传等于给用户一屏机器话，所以只留尾部。
const SELF_CHECK_TAIL_LINES: usize = 8;

fn build_routing(
    rules: &[RoutingRule],
    selected_tag: &str,
    sentinel_dns: Option<&str>,
) -> Value {
    let mut compiled: Vec<Value> = Vec::new();

    // 1) DNS 劫持必须最先，而且**必须限定在发往哨兵地址的查询上**。
    //
    // 早先这里只写了 `"port": "53"`，即「任何来源、任何目的的 53 端口」。
    // 那条规则会连**内核自己的上游解析**一起吞掉：
    //
    //   内核 DNS 模块要查 223.5.5.5:53（国内域名走国内解析器）
    //     → 它的 UDP 客户端用的是**裸 socket**，不走 dispatcher
    //     → TUN 模式下这个包按默认路由又掉回 utun
    //     → 重新进入 tun 入站，撞上这条 `port: 53` 规则
    //     → 被塞回 dns-out，也就是回到 DNS 模块自己
    //
    // 结果是**国内解析这条腿从来没出过机器**：国内域名查不到 → 回退到 DoH
    // → DoH 是走 dispatcher 的（日志里的 `[dns-module -> node-...]`）
    // → 于是所有解析都压到节点上 → 节点一慢就是满屏
    // `context deadline exceeded` / `record not found`。
    //
    // 这也解释了为什么它在系统代理模式下测不出来：没有 tun，裸 UDP 包
    // 正常从 en0 出去，国内解析 1ms 就回来了。
    //
    // 限定成哨兵地址之后就各归各位：
    //   * 客户端的 DNS 都发给哨兵（系统 DNS 被我们改成它）→ 照旧被劫持；
    //   * 内核自己的 223.5.5.5:53 不被劫持 → 落到 `preset-cn-ip`
    //     → `direct` 出站（绑定了物理网卡）→ **逃出隧道**；
    //   * 内核的 DoH（1.1.1.1:443）→ 落到兜底 → 走节点（本来的意图）。
    let mut hijack = json!({
        "type": "field",
        "port": "53",
        "outboundTag": "dns-out",
        "ruleTag": RULE_TAG_DNS_HIJACK
    });
    if let Some(sentinel) = sentinel_dns {
        hijack["ip"] = json!([sentinel]);
    }
    compiled.push(hijack);

    // 2) API 流量。
    compiled.push(json!({
        "type": "field",
        "inboundTag": ["api"],
        "outboundTag": "api",
        "ruleTag": RULE_TAG_API
    }));

    // 3) 用户规则（预设 + 自定义，已按优先级排好）。
    compiled.extend(routing::compile(rules, selected_tag));

    // 4) 兜底：没被任何规则命中时走当前节点。
    compiled.push(json!({
        "type": "field",
        "network": "tcp,udp",
        "outboundTag": selected_tag,
        "ruleTag": RULE_TAG_FALLBACK
    }));

    // 5) **最终不变量：整份 `rules` 的 `ruleTag` 必须唯一**，否则 Xray 在
    //    `app/router` 阶段拒绝启动（用户报的原文：
    //    `failed to create server > app/router: duplicate ruleTag preset-private`）。
    //
    //    这一层才看得见**全部三处来源**：预设规则、自定义规则、以及上面 1) 2) 4)
    //    追加的内部规则。用户自定义规则的 id 撞上内部 tag 时 `merge_rules` 看不到它们
    //    （真实核心实测 `internal-api` / `internal-fallback` / `internal-dns-hijack`
    //    三个都 exit 23），所以唯一化必须落在最终数组上。
    uniquify_rule_tag_values(&mut compiled);

    json!({
        // IPIfNonMatch：先按域名规则匹配，未命中再解析成 IP 匹配。
        // 兼顾“域名规则精准”与“IP 规则也能命中”两种诉求。
        "domainStrategy": "IPIfNonMatch",
        "domainMatcher": "hybrid",
        "rules": compiled
    })
}

fn build_policy() -> Value {
    json!({
        "levels": {
            "0": {
                "statsUserUplink": true,
                "statsUserDownlink": true,
                "handshake": 4,
                "connIdle": 300,
                "uplinkOnly": 2,
                "downlinkOnly": 5
            }
        },
        "system": {
            "statsInboundUplink": true,
            "statsInboundDownlink": true,
            "statsOutboundUplink": true,
            "statsOutboundDownlink": true
        }
    })
}

/// 把预设与自定义规则合并成最终顺序：
/// 预设在前（它们包含“私有地址直连”这类必须优先的规则），自定义在后。
///
/// **并保证 `RoutingRule::id` 唯一**（`id` 会原样写进配置的 `ruleTag`）——
/// 重复的 `ruleTag` 会让 Xray 在 `app/router` 阶段拒绝启动：用户报的原文就是
/// `failed to create server > app/router: duplicate ruleTag preset-private`。
/// 用户的 `custom_rules` 里带着 `preset-private` 这种**与预设同名**的 id 时必撞
/// （本机用户真实数据：`[preset-private, preset-ads, google-to-us,
/// preset-cn-domain, preset-cn-ip]` × `bypass_mainland` ⇒ 4 个 id 各两次）。
///
/// 这一层只看得见「预设 + 自定义」；App 自己追加的内部规则 tag（`internal-*`）
/// 由 [`uniquify_rule_tag_values`] 在**最终 rules 数组**上兜底。
pub fn merge_rules(s: &AppSettings) -> Vec<RoutingRule> {
    merge_rules_with_intent(s, &[], &[])
}

/// 预设 + **意图规则** + 自定义，按优先级拼成一份规则表。
///
/// # 意图规则插在哪，以及为什么
///
/// ```text
/// [preset-private]      ← 永远最先（否则路由器/NAS 不可达）
/// [intent-allow-*]      ← 用户纠正（必须先于 block，才能纠正误杀）
/// [intent-block-*]      ← Jev 判定的投放/追踪端点
/// [preset-ads]          ← geosite:category-ads-all（L0 静态名单）
/// [preset-cn-domain] …
/// [自定义规则]
/// ```
///
/// 三条顺序理由（每条都对应一个失败模式）：
///
/// * **`allow` 必须早于 `block`** —— 反过来的话，用户点"这个拦错了"之后什么都不会发生；
/// * **意图规则必须早于 `preset-cn-domain` / `preset-ads`** —— 否则被 `direct` 规则先命中，
///   意图判定花了钱却永远不生效（与 `preset-ads` 必须早于 `preset-cn-domain` 同一条理由）；
/// * **插在 `preset-private` 之后** —— 私有地址直连是唯一一条不允许被覆盖的规则。
///
/// `Custom` 预设下没有任何预设规则，于是意图两带落在**自定义规则之前**：
/// 意图过滤属于"预设"这一层，用户要纠正就用放行纠正（`allow_overrides`），
/// 而不是期望自定义规则能压过它。
///
/// `intent_allow` / `intent_block` 由 `xt-intent` 的 `materialize()` 产出；
/// 本函数**不依赖那个 crate**（会成环），只把 `RoutingRule` 当数据。
pub fn merge_rules_with_intent(
    s: &AppSettings,
    intent_allow: &[RoutingRule],
    intent_block: &[RoutingRule],
) -> Vec<RoutingRule> {
    let mut rules = if s.routing_preset == RoutingPreset::Custom {
        s.custom_rules.clone()
    } else {
        let mut rules = routing::preset_rules(s.routing_preset);
        rules.extend(s.custom_rules.iter().cloned());
        rules
    };

    if !intent_allow.is_empty() || !intent_block.is_empty() {
        let at = rules
            .iter()
            .position(|r| r.id == "preset-private")
            .map(|i| i + 1)
            .unwrap_or(0);
        let mut band: Vec<RoutingRule> = Vec::with_capacity(intent_allow.len() + intent_block.len());
        band.extend(intent_allow.iter().cloned());
        band.extend(intent_block.iter().cloned());
        rules.splice(at..at, band);
    }

    let mut ids: Vec<String> = rules.iter().map(|r| r.id.clone()).collect();
    if uniquify_tags(&mut ids) > 0 {
        for (rule, id) in rules.iter_mut().zip(ids) {
            rule.id = id;
        }
    }
    rules
}

/// 就地把重复的 tag 改成确定性的唯一形式：**首次出现保持原样**，之后依次加 `#2`、`#3`…
///
/// # 为什么不是「同名即同规则、合并/丢弃一条」
///
/// 那会**改变路由语义**：两条规则可能只是 tag 相同、条件完全不同（用户常把预设规则
/// 复制出来改条件）；丢任何一条都会让流量走错出口。而 `ruleTag` **只出现在 Xray
/// 日志里**（`Hit route rule: [tag]`）用于排障，**不参与规则匹配** —— 所以加后缀
/// **不改变路由行为**，只让「是哪一条命中」在日志里可区分。同理：这里既不删规则，
/// 也不回写用户的 `settings.json`。
///
/// 返回被改名的条数（0 = 本来就没有重复）。
fn uniquify_tags(tags: &mut [String]) -> usize {
    let mut used: HashSet<String> = HashSet::with_capacity(tags.len());
    let mut renamed = 0usize;
    for tag in tags.iter_mut() {
        if used.insert(tag.clone()) {
            continue;
        }
        let base = tag.clone();
        let mut n = 2usize;
        let unique = loop {
            let candidate = format!("{base}#{n}");
            if used.insert(candidate.clone()) {
                break candidate;
            }
            n += 1;
        };
        *tag = unique;
        renamed += 1;
    }
    renamed
}

/// 对**最终** `routing.rules` 数组做唯一化（就地改 `ruleTag`）。
///
/// 这一层是硬保证：它同时覆盖预设规则、自定义规则与内部规则。
fn uniquify_rule_tag_values(rules: &mut [Value]) {
    let mut tags: Vec<String> = rules
        .iter()
        .filter_map(|r| r.get("ruleTag").and_then(Value::as_str).map(str::to_string))
        .collect();
    if uniquify_tags(&mut tags) == 0 {
        return;
    }
    let mut renamed = tags.into_iter();
    for rule in rules.iter_mut() {
        // 只回填**本来就有 `ruleTag`** 的规则，别给没有 tag 的规则凭空造一个。
        if rule.get("ruleTag").and_then(Value::as_str).is_some() {
            if let Some(tag) = renamed.next() {
                rule["ruleTag"] = Value::String(tag);
            }
        }
    }
}

/// 一处重复的 `ruleTag`（诊断用；见 [`duplicate_rule_tags`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleTagConflict {
    /// 重复的 tag。
    pub tag: String,
    /// 它在最终 `rules` 里出现的次数。
    pub count: usize,
    /// 该 tag 来自哪些输入：`内置` / `预设` / `自定义`（按此顺序去重）。
    pub sources: Vec<&'static str>,
}

impl RuleTagConflict {
    /// 人话描述：「`preset-private` 出现 2 次（预设 + 自定义）」。文案与测试共用。
    pub fn describe(&self) -> String {
        let sources = if self.sources.is_empty() {
            "来源未识别".to_string()
        } else {
            self.sources.join(" + ")
        };
        format!("`{}` 出现 {} 次（{}）", self.tag, self.count, sources)
    }
}

/// 已生成的配置里**重复的 `ruleTag`**。
///
/// 正常情况下**恒为空**：`build_routing` 在写盘前已唯一化。非空只可能意味着某条
/// 生成路径绕过了那道唯一化 —— 与其把核心日志整段丢给用户（实测 stderr 为空、
/// stdout 3445 字节 / 35 行，可操作的那句在最后一行），不如指名「哪个 tag / 来自
/// 哪里 / 几次」，见 [`config_self_check_message`]。
pub fn duplicate_rule_tags(settings: &AppSettings, config: &Value) -> Vec<RuleTagConflict> {
    let Some(rules) = config
        .get("routing")
        .and_then(|r| r.get("rules"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for rule in rules {
        if let Some(tag) = rule.get("ruleTag").and_then(Value::as_str) {
            *counts.entry(tag.to_string()).or_insert(0) += 1;
        }
    }
    let preset_ids: Vec<String> = if settings.routing_preset == RoutingPreset::Custom {
        Vec::new()
    } else {
        routing::preset_rules(settings.routing_preset)
            .iter()
            .map(|r| r.id.clone())
            .collect()
    };
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(tag, count)| {
            let mut sources = Vec::new();
            if INTERNAL_RULE_TAGS.contains(&tag.as_str()) {
                sources.push("内置");
            }
            if preset_ids.iter().any(|id| id == &tag) {
                sources.push("预设");
            }
            if settings.custom_rules.iter().any(|r| r.id == tag) {
                sources.push("自定义");
            }
            RuleTagConflict { tag, count, sources }
        })
        .collect()
}

/// 「配置未通过核心自检」时给用户看的文案：**先结论 + 下一步**，再附**截断后**的核心输出。
///
/// 用户原来的观感是一屏机器话（`生成的配置未通过核心自检：<3445 字节原始日志>`），
/// 里面没有「哪个 tag / 来自预设还是自定义 / 共几条」，也没有「怎么办」。
/// `config_json` 是**已生成、待写盘**的配置文本（`build_pretty` 的输出）。
pub fn config_self_check_message(settings: &AppSettings, config_json: &str, raw: &str) -> String {
    let conflicts = serde_json::from_str::<Value>(config_json)
        .ok()
        .map(|config| duplicate_rule_tags(settings, &config))
        .unwrap_or_default();

    let mut out = String::new();
    if conflicts.is_empty() {
        out.push_str(
            "核心拒绝了这份配置（自检未通过）。可先到「设置 → 内核与更新」换一个核心，\
             或到「路由」页把最近改动的规则恢复默认，再试一次。",
        );
    } else {
        out.push_str("规则标识（ruleTag）重复 —— 核心会因此拒绝启动：");
        for conflict in &conflicts {
            out.push_str("\n  · ");
            out.push_str(&conflict.describe());
        }
        out.push_str(
            "\nApp 已自动为**后出现**的那条加后缀（`#2`、`#3`…）保证唯一 —— \
             这**不改变路由行为**（`ruleTag` 只用于日志排障）；\
             若你不想让两条都生效，请在「路由」页删掉其中一条。",
        );
    }

    let lines: Vec<&str> = raw.lines().collect();
    let tail = lines.len().min(SELF_CHECK_TAIL_LINES);
    out.push_str(&format!(
        "\n\n核心原始输出（共 {} 行，只保留末尾 {} 行）：",
        lines.len(),
        tail
    ));
    for line in &lines[lines.len() - tail..] {
        out.push('\n');
        out.push_str(line);
    }
    out
}

// ---------------------------------------------------------------------------
// 节点 -> outbound
// ---------------------------------------------------------------------------

/// 渲染单个节点的 outbound。**这是最容易出错的地方**，字段名必须与 Xray 完全一致。
pub fn node_to_outbound(node: &Node) -> Value {
    let address = &node.address;
    let port = node.port;

    let (protocol, settings) = match &node.protocol {
        Protocol::Vmess { uuid, alter_id, security } => (
            "vmess",
            json!({
                "vnext": [{
                    "address": address,
                    "port": port,
                    "users": [{
                        "id": uuid,
                        "alterId": alter_id,
                        "security": security.as_xray(),
                        "level": 0
                    }]
                }]
            }),
        ),
        Protocol::Vless { uuid, flow, encryption } => {
            let mut user = json!({
                "id": uuid,
                "encryption": encryption,
                "level": 0
            });
            if !flow.is_empty() {
                user["flow"] = Value::String(flow.clone());
            }
            ("vless", json!({ "vnext": [{ "address": address, "port": port, "users": [user] }] }))
        }
        Protocol::Trojan { password } => (
            "trojan",
            json!({
                "servers": [{ "address": address, "port": port, "password": password, "level": 0 }]
            }),
        ),
        Protocol::Shadowsocks { method, password, uot } => (
            "shadowsocks",
            json!({
                "servers": [{
                    "address": address,
                    "port": port,
                    "method": method,
                    "password": password,
                    "uot": uot,
                    "level": 0
                }]
            }),
        ),
        Protocol::Socks { username, password } => {
            let mut server = json!({ "address": address, "port": port, "level": 0 });
            if !username.is_empty() {
                server["users"] = json!([{ "user": username, "pass": password, "level": 0 }]);
            }
            ("socks", json!({ "servers": [server] }))
        }
        Protocol::Http { username, password } => {
            let mut server = json!({ "address": address, "port": port, "level": 0 });
            if !username.is_empty() {
                server["users"] = json!([{ "user": username, "pass": password, "level": 0 }]);
            }
            ("http", json!({ "servers": [server] }))
        }
    };

    let mut outbound = Map::new();
    outbound.insert("tag".into(), Value::String(node.outbound_tag()));
    outbound.insert("protocol".into(), Value::String(protocol.into()));
    outbound.insert("settings".into(), settings);
    outbound.insert("streamSettings".into(), stream_settings(node));
    if let Some(mux) = &node.mux {
        if mux.enabled {
            outbound.insert(
                "mux".into(),
                json!({
                    "enabled": true,
                    "concurrency": mux.concurrency,
                    "xudpConcurrency": if mux.xudp_concurrency == 0 { 16 } else { mux.xudp_concurrency },
                    "xudpProxyUDP443": "reject"
                }),
            );
        }
    }
    Value::Object(outbound)
}

fn stream_settings(node: &Node) -> Value {
    let mut ss = Map::new();
    ss.insert("network".into(), Value::String(node.transport.network().into()));

    let tls = &node.tls;
    let security = tls.effective_security();
    ss.insert("security".into(), Value::String(security.into()));

    // SNI 缺省时回退到节点地址 —— 绝大多数服务端就是这么配的。
    let server_name = if tls.server_name.is_empty() { node.address.clone() } else { tls.server_name.clone() };

    match security {
        "tls" => {
            let mut t = Map::new();
            t.insert("serverName".into(), Value::String(server_name));
            t.insert("allowInsecure".into(), Value::Bool(tls.allow_insecure));
            if !tls.alpn.is_empty() {
                t.insert("alpn".into(), json!(tls.alpn));
            }
            if !tls.fingerprint.is_empty() {
                // uTLS 指纹是抗主动探测的关键，不要因为字段可选就丢掉。
                t.insert("fingerprint".into(), Value::String(tls.fingerprint.clone()));
            }
            ss.insert("tlsSettings".into(), Value::Object(t));
        }
        "reality" => {
            let r = tls.reality.as_ref();
            let mut t = Map::new();
            t.insert("serverName".into(), Value::String(server_name));
            t.insert(
                "fingerprint".into(),
                Value::String(if tls.fingerprint.is_empty() { "chrome".into() } else { tls.fingerprint.clone() }),
            );
            t.insert("publicKey".into(), Value::String(r.map(|x| x.public_key.clone()).unwrap_or_default()));
            t.insert("shortId".into(), Value::String(r.map(|x| x.short_id.clone()).unwrap_or_default()));
            t.insert(
                "spiderX".into(),
                Value::String(r.map(|x| x.spider_x.clone()).filter(|x| !x.is_empty()).unwrap_or_else(|| "/".into())),
            );
            t.insert("show".into(), Value::Bool(false));
            ss.insert("realitySettings".into(), Value::Object(t));
        }
        _ => {
            ss.insert("security".into(), Value::String("none".into()));
        }
    }

    match &node.transport {
        Transport::Tcp => {}
        Transport::WebSocket { path, host } => {
            let mut w = Map::new();
            w.insert("path".into(), Value::String(path.clone()));
            if !host.is_empty() {
                // Host 头伪装：留空时让 Xray 用 SNI，避免写出空头。
                w.insert("headers".into(), json!({ "Host": host }));
            }
            ss.insert("wsSettings".into(), Value::Object(w));
        }
        Transport::Grpc { service_name, multi_mode } => {
            ss.insert(
                "grpcSettings".into(),
                json!({ "serviceName": service_name, "multiMode": multi_mode }),
            );
        }
        Transport::HttpUpgrade { path, host } => {
            ss.insert(
                "httpupgradeSettings".into(),
                json!({ "path": path, "host": host }),
            );
        }
        Transport::Xhttp { path, host, mode } => {
            ss.insert(
                "xhttpSettings".into(),
                json!({ "path": path, "host": host, "mode": mode }),
            );
        }
        Transport::Quic { key, security } => {
            ss.insert("quicSettings".into(), json!({ "key": key, "security": security }));
        }
        Transport::Kcp { header_type, seed } => {
            let mut k = json!({ "header": { "type": header_type } });
            if !seed.is_empty() {
                k["seed"] = Value::String(seed.clone());
            }
            ss.insert("kcpSettings".into(), k);
        }
        Transport::Http { host, path } => {
            let hosts = if host.is_empty() { Vec::new() } else { vec![host.clone()] };
            ss.insert("httpSettings".into(), json!({ "host": hosts, "path": path }));
        }
    }

    Value::Object(ss)
}

/// 校验 TLS 配置的常见错误，返回人类可读的警告（不阻断运行）。
pub fn lint_node(node: &Node) -> Vec<String> {
    let mut out = Vec::new();
    if node.transport.requires_tls() && !node.tls.effective_enabled() {
        out.push(format!("[{}] 使用 {} 传输但未启用 TLS，Xray 会启动失败", node.name, node.transport.network()));
    }
    if node.tls.reality.is_some() && node.tls.reality.as_ref().map(|r| r.public_key.is_empty()).unwrap_or(false) {
        out.push(format!("[{}] REALITY 的 publicKey 为空", node.name));
    }
    if node.address.is_empty() || node.port == 0 {
        out.push(format!("[{}] 地址或端口为空", node.name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NodeSource, ProxyMode, TlsSettings};
    use crate::routing::{MatchCondition, RuleAction};

    fn node() -> Node {
        let mut n = Node {
            id: String::new(),
            name: "jp".into(),
            address: "jp.example.com".into(),
            port: 443,
            protocol: Protocol::Vless {
                uuid: "u-1".into(),
                flow: "xtls-rprx-vision".into(),
                encryption: "none".into(),
            },
            transport: Transport::Tcp,
            tls: TlsSettings {
                enabled: true,
                server_name: String::new(),
                allow_insecure: false,
                alpn: vec!["h2".into()],
                fingerprint: "chrome".into(),
                reality: None,
            },
            mux: None,
            source: NodeSource::Manual,
            tags: vec![],
            raw_uri: None,
        };
        n.refresh_id();
        n
    }

    fn settings() -> AppSettings {
        AppSettings {
            mode: ProxyMode::Tun,
            ..Default::default()
        }
    }

    fn tun_profile(s: &AppSettings) -> InboundProfile {
        InboundProfile::Tun(tun_inbound_spec(s, Some("en0"), true))
    }

    // -----------------------------------------------------------------------
    // 意图过滤：规则插入位置（顺序即优先级，错了就是静默失效）
    // -----------------------------------------------------------------------

    fn intent_rule(id: &str, domain: &str, then: RuleAction) -> RoutingRule {
        RoutingRule::new(
            id,
            format!("测试规则 {id}"),
            MatchCondition {
                domains: vec![format!("full:{domain}")],
                inbound_tags: vec!["tun".into()],
                ..Default::default()
            },
            then,
        )
    }

    /// 生效顺序里每个 id 的位置。
    fn order_of(rules: &[RoutingRule], id: &str) -> usize {
        rules
            .iter()
            .position(|r| r.id == id)
            .unwrap_or_else(|| panic!("规则表里没有 {id}：{:?}", rules.iter().map(|r| &r.id).collect::<Vec<_>>()))
    }

    #[test]
    fn intent_rules_land_after_private_and_before_ads_and_cn() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::BypassMainland;
        let allow = vec![intent_rule("intent-allow-ok.example", "ok.example", RuleAction::Direct)];
        let block = vec![intent_rule("intent-block-ads.example", "ads.example", RuleAction::Block)];

        let rules = merge_rules_with_intent(&s, &allow, &block);

        let private = order_of(&rules, "preset-private");
        let allow_at = order_of(&rules, "intent-allow-ok.example");
        let block_at = order_of(&rules, "intent-block-ads.example");
        let ads = order_of(&rules, "preset-ads");
        let cn = order_of(&rules, "preset-cn-domain");

        assert!(private < allow_at, "私有地址直连必须永远最先");
        assert!(allow_at < block_at, "用户放行必须早于拦截（否则纠正无效）");
        assert!(block_at < ads, "意图拦截必须早于静态广告名单");
        assert!(block_at < cn, "意图拦截必须早于大陆直连（否则被 direct 先命中）");
        assert!(ads < cn, "既有的顺序理由不变");
    }

    #[test]
    fn intent_bands_are_contiguous_and_ordered_allow_then_block() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::BypassMainland;
        let allow = vec![
            intent_rule("intent-allow-a.example", "a.example", RuleAction::Direct),
            intent_rule("intent-allow-b.example", "b.example", RuleAction::Proxy { outbound: None }),
        ];
        let block = vec![intent_rule("intent-block-c.example", "c.example", RuleAction::Block)];

        let rules = merge_rules_with_intent(&s, &allow, &block);
        let allow_at = order_of(&rules, "intent-allow-a.example");
        assert_eq!(rules[allow_at + 1].id, "intent-allow-b.example");
        assert_eq!(rules[allow_at + 2].id, "intent-block-c.example");
    }

    #[test]
    fn no_intent_rules_means_the_table_is_unchanged() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::BypassMainland;
        let base = merge_rules(&s);
        let with_empty = merge_rules_with_intent(&s, &[], &[]);
        assert_eq!(base, with_empty, "没有意图规则时不许改变任何一条既有规则");
    }

    #[test]
    fn custom_preset_puts_intent_rules_before_user_rules() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::Custom;
        s.custom_rules = vec![intent_rule("mine", "mine.example", RuleAction::Proxy { outbound: None })];
        let block = vec![intent_rule("intent-block-ads.example", "ads.example", RuleAction::Block)];

        let rules = merge_rules_with_intent(&s, &[], &block);
        assert_eq!(rules[0].id, "intent-block-ads.example", "Custom 预设下意图规则也属于「预设」那一层");
        assert_eq!(rules[1].id, "mine");
    }

    #[test]
    fn direct_all_still_gets_the_intent_block_before_the_catch_all() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::DirectAll;
        let block = vec![intent_rule("intent-block-ads.example", "ads.example", RuleAction::Block)];

        let rules = merge_rules_with_intent(&s, &[], &block);
        // 预设只有一条 catch-all 直连；意图拦截必须在它之前，否则永远不会命中。
        assert_eq!(rules[0].id, "intent-block-ads.example");
        assert_eq!(rules.last().unwrap().id, "preset-direct-all");
    }

    #[test]
    fn intent_rules_survive_tag_uniquification_and_do_not_collide() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::BypassMainland;
        // 用户自定义规则故意撞上意图规则的 id。
        s.custom_rules = vec![intent_rule(
            "intent-block-ads.example",
            "other.example",
            RuleAction::Direct,
        )];
        let block = vec![intent_rule("intent-block-ads.example", "ads.example", RuleAction::Block)];

        let rules = merge_rules_with_intent(&s, &[], &block);
        let mut ids: Vec<String> = rules.iter().map(|r| r.id.clone()).collect();
        let total = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), total, "ruleTag 必须全唯一，否则核心拒启（v0.8.37 的 P0）");
        // 而且只改 tag、不改条件：两条规则都还在，各自的条件未变。
        let mine = rules.iter().find(|r| r.when.domains == vec!["full:other.example".to_string()]).unwrap();
        assert_eq!(mine.then, RuleAction::Direct);
        let intent = rules.iter().find(|r| r.when.domains == vec!["full:ads.example".to_string()]).unwrap();
        assert_eq!(intent.then, RuleAction::Block);
    }

    #[test]
    fn an_intent_block_compiles_to_a_blackhole_rule() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::BypassMainland;
        let block = vec![intent_rule("intent-block-ads.example", "ads.example", RuleAction::Block)];
        let rules = merge_rules_with_intent(&s, &[], &block);
        let compiled = routing::compile(&rules, "node-x");

        let hit = compiled
            .iter()
            .find(|r| r["ruleTag"] == "intent-block-ads.example")
            .expect("意图拦截规则必须出现在编译结果里");
        assert_eq!(hit["outboundTag"], "block");
        assert_eq!(hit["domain"][0], "full:ads.example");
        assert_eq!(hit["inboundTag"][0], "tun");
        // Xray 要求每条 field 规则至少有一个生效字段；这里靠 domain 满足。
        assert!(hit["domain"].as_array().is_some_and(|a| !a.is_empty()));
    }

    #[test]
    fn dns_hijack_is_the_first_rule() {
        let s = settings();
        let nodes = vec![node()];
        let rules = merge_rules(&s);
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &nodes,
            selected: Some(&nodes[0].id),
            rules: &rules,
            profile: tun_profile(&s),
            physical_interface: Some("en0"),
        });
        let r = &cfg["routing"]["rules"];
        assert_eq!(r[0]["ruleTag"], "internal-dns-hijack");
        assert_eq!(r[0]["outboundTag"], "dns-out");
        // 最后一条永远是兜底
        let last = r.as_array().unwrap().last().unwrap();
        assert_eq!(last["ruleTag"], "internal-fallback");
        assert_eq!(last["outboundTag"], nodes[0].outbound_tag());
    }

    /// DNS 劫持**必须**限定在哨兵地址上，不能是「任何来源、任何目的的 53」。
    ///
    /// 这条钉住的是一个把 DNS 彻底打死的真实故障：内核自己的上游解析
    /// （国内域名 → 223.5.5.5:53）用的是裸 UDP socket，不走 dispatcher；
    /// TUN 模式下这个包按默认路由掉回 utun，重新进入 tun 入站，然后被
    /// 宽泛的 `port: 53` 规则塞回 dns-out —— 即回到 DNS 模块自己。
    ///
    /// 后果是国内解析这条腿从未离开过机器，所有解析都回退到走节点的 DoH，
    /// 节点一慢就满屏 `context deadline exceeded`。
    #[test]
    fn dns_hijack_is_scoped_to_the_sentinel() {
        let s = settings();
        let nodes = vec![node()];
        let rules = merge_rules(&s);
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &nodes,
            selected: Some(&nodes[0].id),
            rules: &rules,
            profile: tun_profile(&s),
            physical_interface: Some("en0"),
        });
        let hijack = &cfg["routing"]["rules"][0];
        assert_eq!(
            hijack["ip"],
            json!([s.tun.sentinel_dns]),
            "劫持规则必须限定在哨兵地址，否则内核自己的上游查询也会被吞掉"
        );
        assert_eq!(hijack["port"], "53");
    }

    /// 非 TUN 档位没有哨兵，这时也**不能**退化成「劫持一切 53 端口」。
    #[test]
    fn dns_hijack_has_no_blanket_port_match_in_proxy_mode() {
        let s = settings();
        let nodes = vec![node()];
        let rules = merge_rules(&s);
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &nodes,
            selected: Some(&nodes[0].id),
            rules: &rules,
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        let hijack = &cfg["routing"]["rules"][0];
        assert!(
            hijack.get("ip").is_none(),
            "系统代理模式下不该出现端口 53 的兜底劫持"
        );
    }

    #[test]
    fn socks_inbound_has_udp_enabled() {
        let s = settings();
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: tun_profile(&s),
            physical_interface: Some("en0"),
        });
        let socks = cfg["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["tag"] == "socks")
            .unwrap();
        assert_eq!(socks["settings"]["udp"], true, "TUN 模式的 UDP 中继依赖它");
    }

    #[test]
    fn allow_lan_switches_listen_address() {
        let mut s = settings();
        s.allow_lan = true;
        let profile = tun_profile(&s);
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile, physical_interface: Some("en0") });
        assert_eq!(cfg["inbounds"][0]["listen"], "0.0.0.0");
    }

    #[test]
    fn tls_server_name_falls_back_to_address() {
        let n = node();
        let ob = node_to_outbound(&n);
        assert_eq!(ob["streamSettings"]["security"], "tls");
        assert_eq!(ob["streamSettings"]["tlsSettings"]["serverName"], "jp.example.com");
        assert_eq!(ob["streamSettings"]["tlsSettings"]["fingerprint"], "chrome");
    }

    #[test]
    fn reality_settings_are_emitted() {
        let mut n = node();
        n.tls = TlsSettings {
            enabled: true,
            server_name: "www.apple.com".into(),
            allow_insecure: false,
            alpn: vec![],
            fingerprint: "chrome".into(),
            reality: Some(crate::model::RealitySettings {
                public_key: "PBK".into(),
                short_id: "ab".into(),
                spider_x: "/x".into(),
            }),
        };
        let ob = node_to_outbound(&n);
        let r = &ob["streamSettings"]["realitySettings"];
        assert_eq!(ob["streamSettings"]["security"], "reality");
        assert_eq!(r["publicKey"], "PBK");
        assert_eq!(r["shortId"], "ab");
        assert_eq!(r["spiderX"], "/x");
    }

    #[test]
    fn ws_transport_emits_headers_only_when_host_present() {
        let mut n = node();
        n.transport = Transport::WebSocket { path: "/ws".into(), host: String::new() };
        let ob = node_to_outbound(&n);
        assert_eq!(ob["streamSettings"]["wsSettings"]["path"], "/ws");
        assert!(ob["streamSettings"]["wsSettings"].get("headers").is_none());

        n.transport = Transport::WebSocket { path: "/ws".into(), host: "cdn.example.com".into() };
        let ob = node_to_outbound(&n);
        assert_eq!(ob["streamSettings"]["wsSettings"]["headers"]["Host"], "cdn.example.com");
    }

    #[test]
    fn vmess_security_is_rendered() {
        let mut n = node();
        n.protocol = Protocol::Vmess {
            uuid: "id-1".into(),
            alter_id: 0,
            security: crate::model::VmessSecurity::Chacha20Poly1305,
        };
        let ob = node_to_outbound(&n);
        assert_eq!(ob["protocol"], "vmess");
        assert_eq!(ob["settings"]["vnext"][0]["users"][0]["security"], "chacha20-poly1305");
    }

    #[test]
    fn block_and_direct_outbounds_always_exist() {
        let s = settings();
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: InboundProfile::LocalProxy, physical_interface: None });
        let tags: Vec<String> = cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["tag"].as_str().unwrap().to_string())
            .collect();
        for want in ["direct", "block", "dns-out", "api"] {
            assert!(tags.contains(&want.to_string()), "缺少 outbound: {want}");
        }
    }

    /// `dns-out` **刻意不带 `settings`**：非 A/AAAA 查询走内核默认的「快速空
    /// NOERROR」，实测比任何显式配置都好。这条测试是为了防止以后有人看到
    /// 日志里的 `rejected type ...` 就去给它加 `nonIPQuery` / `rules` ——
    /// 那只会让 DNS 变慢或变卡（见 docs/04 §6.8）。
    #[test]
    fn dns_outbound_has_no_settings_on_purpose() {
        let s = settings();
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        let dns_out = cfg["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["tag"] == "dns-out")
            .expect("应有 dns-out 出站");
        assert_eq!(dns_out["protocol"], "dns");
        assert!(
            dns_out.get("settings").is_none(),
            "dns-out 不该有 settings — 默认行为已是最优，加 nonIPQuery 会把快速空应答变成超时，加 rules+direct 会多一次上游往返"
        );
    }

    #[test]
    fn generated_config_is_valid_json() {
        let s = settings();
        let nodes = vec![node()];
        let profile = tun_profile(&s);
        let text = build_pretty(&CoreConfigInput {
            settings: &s,
            nodes: &nodes,
            selected: Some(&nodes[0].id),
            rules: &merge_rules(&s),
            profile,
            physical_interface: Some("en0"),
        });
        let _: Value = serde_json::from_str(&text).unwrap();
    }

    // ---- 原生 TUN ----

    #[test]
    fn native_tun_inbound_is_emitted_for_tun_profile() {
        let s = settings();
        let profile = tun_profile(&s);
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile, physical_interface: Some("en0") });
        let tun = cfg["inbounds"].as_array().unwrap().iter().find(|i| i["protocol"] == "tun").unwrap();
        assert_eq!(tun["tag"], "tun");
        assert_eq!(tun["settings"]["mtu"], 1500);
        assert_eq!(tun["settings"]["gateway"][0], "198.18.0.1/15");
        assert_eq!(tun["settings"]["autoOutboundsInterface"], "en0");
        // 用 /1 拆分而不是 default，回滚更安全
        let routes = tun["settings"]["autoSystemRoutingTable"].as_array().unwrap();
        assert!(routes.contains(&Value::String("0.0.0.0/1".into())));
        assert!(routes.contains(&Value::String("128.0.0.0/1".into())));
        assert!(!routes.contains(&Value::String("default".into())));
    }

    /// task-93：`direct` 出站**显式**绑物理网卡。
    ///
    /// 这条是 DNS 上游（国内明文 DNS 走 `preset-cn-ip` → `direct`）和直连流量
    /// 逃出隧道的确定性依赖，**不依赖** tun 入站的全局 controller
    /// （那个在 v6 族 socket 上 EINVAL，见 `build_outbounds` 的注释）。
    /// 同时钉住：配置里没有「强制某个出站用 v4 socket 族」这种开关可加。
    #[test]
    fn direct_outbound_binds_the_physical_interface() {
        let s = settings();
        let profile = tun_profile(&s);
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile, physical_interface: Some("en0") });
        let direct = cfg["outbounds"].as_array().unwrap().iter().find(|o| o["tag"] == "direct").unwrap();
        assert_eq!(direct["streamSettings"]["sockopt"]["interface"], "en0");
        assert_eq!(direct["streamSettings"]["sockopt"]["domainStrategy"], "UseIPv4");

        // 拿不到物理网卡时**不要**写出空的 `interface`：宁可明显不绑，
        // 也不要写一个坏值让核心去猜。
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: tun_profile(&s), physical_interface: None });
        let direct = cfg["outbounds"].as_array().unwrap().iter().find(|o| o["tag"] == "direct").unwrap();
        assert!(direct["streamSettings"]["sockopt"].get("interface").is_none());
    }

    /// task-97：`direct` 出站**只解析 IPv4** —— 「国外可以、国内断掉」的修复。
    ///
    /// 依据是本机实测（`app.jsonl` 2026-09-22，按 session id 归并连接）：
    /// 只有 v6 目标改写的连接 42 条里 40 条失败（95.2%），只有 v4 改写的
    /// 408 条里 0 条失败。`UseIP` 会让核心把客户端**已经给出的 v4 目标**
    /// （sniff 成域名之后）重新解析成 AAAA，而这台机器没有可用的 v6。
    #[test]
    fn direct_outbound_never_picks_the_v6_family() {
        let s = settings();
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[node()], selected: None, rules: &[], profile: tun_profile(&s), physical_interface: Some("en0") });
        let outbounds = cfg["outbounds"].as_array().unwrap();
        let direct = outbounds.iter().find(|o| o["tag"] == "direct").unwrap();

        // 正向：就是选定的那个值。
        assert_eq!(
            direct["streamSettings"]["sockopt"]["domainStrategy"], "UseIPv4",
            "direct 出站必须只解析 IPv4：本机没有可用 v6，而 UseIP 会把 v4 目标改写成 AAAA 后全灭"
        );
        // 反向：回到 `UseIP` 就是把这条故障放回来（task-97 实测 95.2% 失败率）。
        assert_ne!(
            direct["streamSettings"]["sockopt"]["domainStrategy"], "UseIP",
            "回到 UseIP 等于放回「国内时不时断掉」；见 build_outbounds 的注释与 task-97 的 before 指标"
        );

        // 境外/代理路径**一律不动**：节点出站没有 sockopt（解析发生在节点侧）。
        let node_ob = outbounds
            .iter()
            .find(|o| o["tag"].as_str().is_some_and(|t| t.starts_with("node-")))
            .expect("给了节点就应当有 node-* 出站");
        let node_sockopt = node_ob["streamSettings"].get("sockopt");
        assert!(
            node_sockopt.is_none() || node_sockopt == Some(&Value::Null),
            "境外路径不许被改动，实际 node 出站的 sockopt = {node_sockopt:?}"
        );

        // DNS 层的 `queryStrategy` 仍然是**用户设置**：我们没有选「在 DNS 层
        // 做 UseIPv4」那条更宽的修法（那会把客户端要的 AAAA 也一起吞掉）。
        assert_eq!(
            cfg["dns"]["queryStrategy"], s.dns.query_strategy,
            "DNS 层的 queryStrategy 必须保持用户设置（本卡只收敛 direct 出站的解析）"
        );
    }

    #[test]
    fn no_tun_inbound_in_local_proxy_profile() {
        let s = settings();
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        assert!(cfg["inbounds"].as_array().unwrap().iter().all(|i| i["protocol"] != "tun"));
    }

    #[test]
    fn ipv6_override_adds_v6_split_routes() {
        let mut s = settings();
        s.tun.ipv6 = xt_proto::Ipv6Mode::Override;
        let profile = tun_profile(&s);
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile, physical_interface: Some("en0") });
        let tun = cfg["inbounds"].as_array().unwrap().iter().find(|i| i["protocol"] == "tun").unwrap();
        let routes: Vec<String> = tun["settings"]["autoSystemRoutingTable"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(routes.contains(&"::/1".to_string()));
        assert!(routes.contains(&"8000::/1".to_string()));
    }

    // ---- Fake-IP ----

    #[test]
    fn fakedns_is_absent_by_default() {
        let s = settings();
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: InboundProfile::LocalProxy, physical_interface: None });
        assert!(cfg.get("fakedns").is_none(), "默认不应写出 fakedns 段");
        let socks = cfg["inbounds"].as_array().unwrap().iter().find(|i| i["tag"] == "socks").unwrap();
        let override_: Vec<&str> = socks["sniffing"]["destOverride"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(!override_.contains(&"fakedns+others"));
    }

    #[test]
    fn fakedns_pool_and_first_dns_server_when_enabled() {
        let mut s = settings();
        s.fakedns.enabled = true;
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: InboundProfile::LocalProxy, physical_interface: None });

        assert_eq!(cfg["fakedns"]["ipPool"], "198.18.0.0/16");
        assert_eq!(cfg["fakedns"]["poolSize"], 65535);
        // fakedns 必须是第一个 DNS 服务器，否则永远轮不到它
        assert_eq!(cfg["dns"]["servers"][0]["address"], "fakedns");

        let socks = cfg["inbounds"].as_array().unwrap().iter().find(|i| i["tag"] == "socks").unwrap();
        assert_eq!(socks["sniffing"]["destOverride"][0], "fakedns+others");
    }

    #[test]
    fn dns_split_by_rule_uses_domain_scoped_servers() {
        let mut s = settings();
        s.dns.mode = DnsHandling::SplitByRule;
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: InboundProfile::LocalProxy, physical_interface: None });
        let servers = cfg["dns"]["servers"].as_array().unwrap();
        let has_cn = servers.iter().any(|x| {
            x.get("domains")
                .and_then(|d| d.as_array())
                .map(|a| a.iter().any(|v| v == "geosite:cn"))
                .unwrap_or(false)
        });
        let has_non_cn = servers.iter().any(|x| {
            x.get("domains")
                .and_then(|d| d.as_array())
                .map(|a| a.iter().any(|v| v == "geosite:geolocation-!cn"))
                .unwrap_or(false)
        });
        assert!(has_cn && has_non_cn, "按规则分流时必须有大陆/非大陆两条解析器");
    }

    /// 同一条 `domains` 下的候选必须**全部**写进配置 —— 那是同层回退链。
    ///
    /// 钉住一个真实故障：之前每个列表只取第一个，于是国外 DoH 超时后回退链
    /// 上只剩国内解析器，被墙域名会被国内解析器接着答出来。隔离实例实测过
    /// 「只取第一个 → 第二台查的是 UDP:223.5.5.5」。见 `build_dns` 的说明。
    #[test]
    fn split_dns_keeps_every_candidate_as_same_tier_fallback() {
        let mut s = settings();
        s.dns.mode = DnsHandling::SplitByRule;
        s.dns.remote_servers = vec![
            "https://1.0.0.1/dns-query".into(),
            "https://1.1.1.1/dns-query".into(),
            "https://8.8.8.8/dns-query".into(),
        ];
        s.dns.direct_servers = vec!["223.5.5.5".into(), "119.29.29.29".into()];
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        let servers = cfg["dns"]["servers"].as_array().unwrap();

        let scoped = |tag: &str| -> Vec<String> {
            servers
                .iter()
                .filter(|x| {
                    x.get("domains")
                        .and_then(|d| d.as_array())
                        .map(|a| a.iter().any(|v| v == tag))
                        .unwrap_or(false)
                })
                .filter_map(|x| x["address"].as_str().map(str::to_string))
                .collect()
        };

        let remote = scoped("geosite:geolocation-!cn");
        assert_eq!(
            remote,
            vec![
                "https://1.0.0.1/dns-query",
                "https://1.1.1.1/dns-query",
                "https://8.8.8.8/dns-query"
            ],
            "国外候选必须按顺序全部进配置，否则超时后没有同层备选",
        );
        assert_eq!(
            scoped("geosite:cn"),
            vec!["223.5.5.5", "119.29.29.29"],
            "国内候选同理",
        );
    }

    /// 列表被清空时仍要有一条可用的解析器 —— 否则配置会没有任何 DNS。
    #[test]
    fn split_dns_falls_back_when_lists_are_empty() {
        let mut s = settings();
        s.dns.mode = DnsHandling::SplitByRule;
        s.dns.remote_servers.clear();
        s.dns.direct_servers.clear();
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        let servers = cfg["dns"]["servers"].as_array().unwrap();
        assert!(!servers.is_empty(), "空列表也要写出硬编码兜底");
        let addresses: Vec<&str> = servers.iter().filter_map(|x| x["address"].as_str()).collect();
        assert!(addresses.contains(&"223.5.5.5"), "{addresses:?}");
        assert!(
            addresses.contains(&"https://1.1.1.1/dns-query"),
            "{addresses:?}"
        );
    }

    /// 分流的解析器**不能**带 `expectIPs`。
    ///
    /// 钉住一个实测到的故障：给大陆解析器加 `expectIPs: ["geoip:cn"]` 之后，
    /// 223.5.5.5 在 1.2ms 内返回了正确答案，却因为地址不在 `geoip:cn`
    /// （`api.deepseek.com` → AWS 的 3.173.21.63）被判为
    /// `features/dns: empty response` 丢弃，随后串行回退到 DoH。
    /// 一次查询从 1ms 变成 450ms 起步，回退链叠上节点抖动就成了
    /// 满屏的 `context canceled`。
    #[test]
    fn split_dns_servers_have_no_expect_ips() {
        let mut s = settings();
        s.dns.mode = DnsHandling::SplitByRule;
        let cfg = build(&CoreConfigInput {
            settings: &s,
            nodes: &[],
            selected: None,
            rules: &[],
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        });
        for server in cfg["dns"]["servers"].as_array().unwrap() {
            assert!(
                server.get("expectIPs").is_none(),
                "分流解析器不能写 expectIPs，否则会把正确的答案丢掉：{server}"
            );
        }
    }

    /// 默认 DNS 模式必须是按规则分流。
    ///
    /// 0.1.0 的默认是「全部走代理解析」，实测每个查询经节点约 450ms，
    /// 且完全依赖节点可用性。旧设置由 `AppSettings::migrate` 一次性改过来。
    #[test]
    fn default_dns_mode_is_split_by_rule() {
        assert_eq!(AppSettings::default().dns.mode, DnsHandling::SplitByRule);
    }

    #[test]
    fn api_services_include_routing_service() {
        let s = settings();
        let cfg = build(&CoreConfigInput { settings: &s, nodes: &[], selected: None, rules: &[], profile: InboundProfile::LocalProxy, physical_interface: None });
        let services: Vec<&str> = cfg["api"]["services"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(services.contains(&"HandlerService"));
        assert!(services.contains(&"StatsService"));
        assert!(services.contains(&"RoutingService"));
    }

    #[test]
    fn lint_catches_missing_tls_for_quic() {
        let mut n = node();
        n.transport = Transport::Quic { key: String::new(), security: String::new() };
        n.tls = TlsSettings::default();
        assert!(!lint_node(&n).is_empty());
    }

    // -----------------------------------------------------------------------
    // task-165（P0 启动阻断）：`ruleTag` 必须唯一
    //
    // 用户报的原文：`Failed to start: main: failed to create server > app/router:
    // duplicate ruleTag preset-private` —— 核心直接起不来。根因是 `merge_rules`
    // 把「预设 + 自定义」原样拼接，而用户 `custom_rules` 里带着与预设同名的 id。
    // -----------------------------------------------------------------------

    /// 用户真实 `settings.json` 里 `custom_rules` 的 **id 形态**（只取 id，无隐私）。
    const USER_RULE_IDS: [&str; 5] = [
        "preset-private",
        "preset-ads",
        "google-to-us",
        "preset-cn-domain",
        "preset-cn-ip",
    ];

    /// 造一条「有真实条件」的规则：改名不许把条件和出站一起改掉。
    fn rule(id: &str) -> RoutingRule {
        RoutingRule::new(
            id,
            id,
            MatchCondition {
                domains: vec!["example.com".into()],
                ..Default::default()
            },
            RuleAction::Direct,
        )
    }

    fn user_shape(preset: RoutingPreset) -> AppSettings {
        let mut s = settings();
        s.routing_preset = preset;
        s.custom_rules = USER_RULE_IDS.iter().map(|id| rule(id)).collect();
        s
    }

    /// 走 App 自己的生成路径（`merge_rules` → `build_pretty`），拿到最终写盘的配置。
    fn config_of(s: &AppSettings) -> Value {
        let rules = merge_rules(s);
        serde_json::from_str(&build_pretty(&CoreConfigInput {
            settings: s,
            nodes: &[],
            selected: None,
            rules: &rules,
            profile: InboundProfile::LocalProxy,
            physical_interface: None,
        }))
        .expect("生成的配置是 JSON")
    }

    fn tags_of(config: &Value) -> Vec<String> {
        config["routing"]["rules"]
            .as_array()
            .expect("routing.rules")
            .iter()
            .map(|r| {
                r["ruleTag"]
                    .as_str()
                    .unwrap_or_else(|| panic!("每条规则都要有 ruleTag：{r}"))
                    .to_string()
            })
            .collect()
    }

    fn duplicated_tags(tags: &[String]) -> Vec<String> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for tag in tags {
            *counts.entry(tag.as_str()).or_insert(0) += 1;
        }
        counts
            .into_iter()
            .filter(|(_, n)| *n > 1)
            .map(|(tag, _)| tag.to_string())
            .collect()
    }

    /// 仿真实核心的失败输出：35 行、末尾是可操作的那一句（实测 stdout 3445 字节）。
    fn fake_core_log_tail(last: &str) -> String {
        (1..=34)
            .map(|i| {
                format!(
                    "[Debug] loading geo data segment {i} from /opt/xray/binaries/geosite.dat (asset #{i})"
                )
            })
            .chain(std::iter::once(last.to_string()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// **P0 复现形态**：用户那份 `custom_rules` + `bypass_mainland`。
    ///
    /// 先用**修前的合并写法**证明用例忠实复现现场（4 个 id 各 ×2），再断言修后：
    /// 条数不减、顺序不变、id 全唯一。
    #[test]
    fn user_rule_shape_with_bypass_mainland_is_uniquified_without_losing_rules() {
        let s = user_shape(RoutingPreset::BypassMainland);

        // 修前的合并写法（预设 extend 自定义）—— 复现用户现场，证明本用例不是空壳。
        let mut before = routing::preset_rules(RoutingPreset::BypassMainland);
        before.extend(s.custom_rules.iter().cloned());
        let before_ids: Vec<String> = before.iter().map(|r| r.id.clone()).collect();
        assert_eq!(
            duplicated_tags(&before_ids),
            vec!["preset-ads", "preset-cn-domain", "preset-cn-ip", "preset-private"],
            "修前必须复现 4 个重复 id（与用户日志 `duplicate ruleTag preset-private` 同形）"
        );

        let merged = merge_rules(&s);
        assert_eq!(
            merged.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
            vec![
                "preset-private",
                "preset-ads",
                "preset-proxy-google",
                "preset-cn-domain",
                "preset-cn-ip",
                "preset-private#2",
                "preset-ads#2",
                "google-to-us",
                "preset-cn-domain#2",
                "preset-cn-ip#2",
            ],
            "顺序必须是「预设在前、自定义在后」，冲突的**自定义**那条加后缀"
        );
        assert_eq!(merged.len(), 10, "一条都不能少（预设 5 + 自定义 5）");
    }

    /// 最终 `rules` 数组的 `ruleTag` 必须唯一 —— 5 个预设 × 5 种撞法，
    /// 含**与 App 内部 tag 撞名**（`internal-*`）与**自定义规则内部重复**。
    #[test]
    fn generated_config_rule_tags_are_unique_for_every_preset_and_collision_shape() {
        let shapes: [(&str, &[&str]); 5] = [
            ("无同 id", &[]),
            ("部分同 id", &["preset-private"]),
            ("全部同 id", &USER_RULE_IDS),
            ("撞内部 tag", &["internal-api", "internal-fallback", "internal-dns-hijack"]),
            ("自定义内部重复", &["dup", "dup"]),
        ];
        let presets = [
            RoutingPreset::GlobalProxy,
            RoutingPreset::BypassMainland,
            RoutingPreset::WhitelistProxy,
            RoutingPreset::DirectAll,
            RoutingPreset::Custom,
        ];
        for preset in presets {
            for (label, ids) in shapes {
                let mut s = settings();
                s.routing_preset = preset;
                s.custom_rules = ids.iter().map(|id| rule(id)).collect();
                let expected = 3
                    + if preset == RoutingPreset::Custom {
                        0
                    } else {
                        routing::preset_rules(preset).len()
                    }
                    + ids.len();
                let tags = tags_of(&config_of(&s));
                assert_eq!(tags.len(), expected, "条数不能变（{label} / {preset:?}）");
                assert!(
                    duplicated_tags(&tags).is_empty(),
                    "ruleTag 必须全唯一（{label} / {preset:?}）：{tags:?}"
                );
            }
        }
    }

    /// 撞上**内部规则** tag 的自定义规则：内部规则保住原名，用户那条加后缀；
    /// 而且**只有 tag 变了** —— 出站与条件一字未动（这正是「改名不影响路由语义」）。
    #[test]
    fn custom_rule_colliding_with_an_internal_tag_keeps_its_routing_semantics() {
        let mut s = settings();
        s.routing_preset = RoutingPreset::GlobalProxy;
        s.custom_rules = vec![rule("internal-api")];
        let config = config_of(&s);
        let rules = config["routing"]["rules"].as_array().unwrap();
        let api: Vec<&Value> = rules
            .iter()
            .filter(|r| r["ruleTag"].as_str().unwrap_or("").starts_with("internal-api"))
            .collect();
        assert_eq!(api.len(), 2, "{rules:?}");
        assert_eq!(api[0]["ruleTag"], "internal-api");
        assert_eq!(
            api[0]["outboundTag"], "api",
            "内部 API 规则必须保住原名，否则 API 入站会指向错地方"
        );
        assert_eq!(api[1]["ruleTag"], "internal-api#2");
        assert_eq!(api[1]["outboundTag"], "direct", "改名不许动出站");
        assert_eq!(api[1]["domain"], json!(["example.com"]), "改名不许动条件");
    }

    /// 确定性：同样输入两次必须给出同样的 tag（用户按界面来回切预设也要能复现）。
    #[test]
    fn rule_tag_uniquification_is_deterministic() {
        let s = user_shape(RoutingPreset::BypassMainland);
        assert_eq!(
            tags_of(&config_of(&s)),
            tags_of(&config_of(&s)),
            "同样的输入必须给出同样的 tag"
        );
    }

    /// 自检失败文案：**先结论 + 下一步**（指名 tag / 来源 / 次数），再附**截断后**的日志。
    /// 用人造配置 + 人造 35 行日志，**不依赖真实核心**。
    #[test]
    fn self_check_message_names_the_duplicate_tag_and_truncates_the_core_log() {
        let s = user_shape(RoutingPreset::BypassMainland);
        let mut config = config_of(&s);
        // 造出「唯一化被绕过」的形态：把第一条预设规则再塞一份（与自定义那条同名）。
        let extra = config["routing"]["rules"][2].clone();
        config["routing"]["rules"].as_array_mut().unwrap().push(extra);

        let conflicts = duplicate_rule_tags(&s, &config);
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].tag, "preset-private");
        assert_eq!(conflicts[0].count, 2);
        assert_eq!(conflicts[0].sources, vec!["预设", "自定义"]);
        assert_eq!(
            conflicts[0].describe(),
            "`preset-private` 出现 2 次（预设 + 自定义）"
        );

        let raw = fake_core_log_tail(
            "Failed to start: main: failed to create server > app/router: duplicate ruleTag preset-private",
        );
        let msg = config_self_check_message(&s, &serde_json::to_string(&config).unwrap(), &raw);

        assert!(msg.starts_with("规则标识（ruleTag）重复"), "第一句必须是结论：{msg}");
        assert!(msg.contains("`preset-private` 出现 2 次（预设 + 自定义）"), "{msg}");
        assert!(msg.contains("路由"), "必须给出下一步：{msg}");
        assert!(msg.contains("Failed to start"), "原始日志的最后一行要留下：{msg}");
        assert!(!msg.contains("asset #1)"), "日志必须截断，别整段刷屏：{msg}");
        assert!(msg.contains("共 35 行，只保留末尾 8 行"), "{msg}");
        assert!(msg.len() < raw.len(), "文案要比原始日志短：{} vs {}", msg.len(), raw.len());
    }

    /// 没有重复 tag 时：仍然截断日志，并给出可操作的下一步。
    #[test]
    fn self_check_message_without_conflicts_still_truncates_and_suggests_a_next_step() {
        let s = settings();
        let config = config_of(&s);
        assert!(duplicate_rule_tags(&s, &config).is_empty());

        let raw = fake_core_log_tail("this rule has no effective fields");
        let msg = config_self_check_message(&s, &serde_json::to_string(&config).unwrap(), &raw);

        assert!(msg.starts_with("核心拒绝了这份配置"), "{msg}");
        assert!(msg.contains("设置 → 内核与更新"), "要给出下一步：{msg}");
        assert!(msg.contains("共 35 行，只保留末尾 8 行"), "{msg}");
        assert!(msg.ends_with("this rule has no effective fields"), "{msg}");
        assert!(msg.len() < raw.len(), "{} vs {}", msg.len(), raw.len());
    }

    /// **真实核心验收**（`#[ignore]`：需要仓库里的 `apps/desktop/binaries/xray`）。
    ///
    /// ```bash
    /// cargo test -p xt-core --lib real_core -- --ignored --nocapture
    /// ```
    ///
    /// 判据：① 修后形态（`ruleTag` 全唯一）核心自检**通过**；② 人为造出重复 tag，
    /// 核心必须**拒绝**且原始报错里出现 `duplicate ruleTag preset-private`；
    /// ③ 把原始输出喂给 `config_self_check_message` 后，用户看到的文案**指名**
    /// 那个 tag、来源与次数，且比原始日志短。
    #[test]
    #[ignore = "需要真实核心二进制（apps/desktop/binaries/xray）"]
    fn real_core_rejects_duplicate_rule_tags_and_the_message_names_them() {
        let core = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../apps/desktop/binaries/xray");
        if !core.exists() {
            eprintln!("跳过：仓库里没有 {}", core.display());
            return;
        }
        let assets = core.parent().expect("binaries 目录");
        let dir = std::env::temp_dir().join("xraytun-task165-real-core");
        std::fs::create_dir_all(&dir).expect("建临时目录");

        let s = user_shape(RoutingPreset::BypassMainland);
        let config = config_of(&s);

        // ① 修后形态：核心自检必须通过。
        let good = dir.join("good.json");
        std::fs::write(&good, serde_json::to_string_pretty(&config).unwrap()).unwrap();
        let out = std::process::Command::new(&core)
            .env("XRAY_LOCATION_ASSET", assets)
            .args(["run", "-test", "-c"])
            .arg(&good)
            .output()
            .expect("跑核心 -test");
        assert!(
            out.status.success(),
            "修后形态必须通过核心自检：stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );

        // ② 造出「唯一化被绕过」的重复 tag：核心必须拒绝，并指名 duplicate ruleTag。
        let mut broken = config.clone();
        let extra = broken["routing"]["rules"][2].clone();
        broken["routing"]["rules"].as_array_mut().unwrap().push(extra);
        let bad = dir.join("bad.json");
        std::fs::write(&bad, serde_json::to_string_pretty(&broken).unwrap()).unwrap();
        let out = std::process::Command::new(&core)
            .env("XRAY_LOCATION_ASSET", assets)
            .args(["run", "-test", "-c"])
            .arg(&bad)
            .output()
            .expect("跑核心 -test");
        assert!(!out.status.success(), "重复 `ruleTag` 必须被核心拒绝");
        // 与 `validate_config` 同口径：stderr 为空时回退 stdout。
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let raw = if stderr.is_empty() {
            String::from_utf8_lossy(&out.stdout).to_string()
        } else {
            stderr
        };
        assert!(
            raw.contains("duplicate ruleTag preset-private"),
            "核心原始报错必须指名重复项：{raw}"
        );

        // ③ 用户最终看到的文案：指名冲突 + 下一步，且日志按「末尾 8 行」截断。
        let msg = config_self_check_message(&s, &serde_json::to_string(&broken).unwrap(), &raw);
        assert!(msg.contains("`preset-private` 出现 2 次（预设 + 自定义）"), "{msg}");
        assert!(msg.contains("路由"), "要给下一步：{msg}");
        let raw_lines = raw.lines().count();
        let tail = raw_lines.min(8);
        assert!(
            msg.contains(&format!("共 {raw_lines} 行，只保留末尾 {tail} 行")),
            "要说明截断了：{msg}"
        );
        // 截断是**有界**的：只在原始输出真的超过 8 行时才断言首行被丢掉
        // （不跟具体核心版本吐多少行较劲）。
        if raw_lines > 8 {
            if let Some(first) = raw.lines().next().filter(|l| !l.trim().is_empty()) {
                assert!(!msg.contains(first), "首行必须被截掉：{msg}");
            }
        }
    }
}
