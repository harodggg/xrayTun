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
    root.insert("routing".into(), build_routing(input.rules, &selected_tag));
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
            let mut out = vec![
                json!({
                    "address": first_or(&d.remote_servers, "https://1.1.1.1/dns-query"),
                    "domains": ["geosite:geolocation-!cn"]
                }),
                json!({
                    "address": first_or(&d.direct_servers, "223.5.5.5"),
                    "domains": ["geosite:cn"]
                }),
            ];
            // 兜底：与前两条同源，保证任何域名都有解析器。
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
    let mut direct_sockopt = serde_json::Map::new();
    direct_sockopt.insert("domainStrategy".into(), "UseIP".into());
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

    // DNS 出站：被路由到这里的 DNS 查询由内核 DNS 模块直接答复。
    out.push(json!({ "tag": "dns-out", "protocol": "dns" }));

    // API 出站：与 `api.tag` 同名，是内核内部约定的管理通道。
    out.push(json!({ "tag": "api", "protocol": "freedom", "settings": {} }));

    Value::Array(out)
}

fn build_routing(rules: &[RoutingRule], selected_tag: &str) -> Value {
    let mut compiled: Vec<Value> = Vec::new();

    // 1) DNS 劫持必须最先。
    compiled.push(json!({
        "type": "field",
        "port": "53",
        "outboundTag": "dns-out",
        "ruleTag": "internal-dns-hijack"
    }));

    // 2) API 流量。
    compiled.push(json!({
        "type": "field",
        "inboundTag": ["api"],
        "outboundTag": "api",
        "ruleTag": "internal-api"
    }));

    // 3) 用户规则（预设 + 自定义，已按优先级排好）。
    compiled.extend(routing::compile(rules, selected_tag));

    // 4) 兜底：没被任何规则命中时走当前节点。
    compiled.push(json!({
        "type": "field",
        "network": "tcp,udp",
        "outboundTag": selected_tag,
        "ruleTag": "internal-fallback"
    }));

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
pub fn merge_rules(s: &AppSettings) -> Vec<RoutingRule> {
    if s.routing_preset == RoutingPreset::Custom {
        return s.custom_rules.clone();
    }
    let mut rules = routing::preset_rules(s.routing_preset);
    rules.extend(s.custom_rules.iter().cloned());
    rules
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
}
