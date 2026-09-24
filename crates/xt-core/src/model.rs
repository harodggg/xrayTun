//! 领域模型：节点、订阅、路由规则、应用设置。
//!
//! 这一层是**与 Xray 配置文件解耦的中间表示（IR）**。分享链接 / Clash YAML /
//! Xray JSON 都先解析成 `Node`，再由 `xray::config` 统一渲染成 Xray 配置。
//! 好处是新增一种订阅格式或新增一种协议时，只需要动 IR 的一侧。

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use xt_proto::Cidr;

/// 节点稳定标识。由协议 + 地址 + 端口 + 凭据派生，保证订阅重复更新时 id 不变。
pub type NodeId = String;

// ===========================================================================
// 协议
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VmessSecurity {
    #[default]
    Auto,
    None,
    Zero,
    Aes128Gcm,
    Chacha20Poly1305,
}

impl VmessSecurity {
    pub fn as_xray(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Zero => "zero",
            Self::Aes128Gcm => "aes-128-gcm",
            Self::Chacha20Poly1305 => "chacha20-poly1305",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Protocol {
    Vmess {
        uuid: String,
        #[serde(default)]
        alter_id: u32,
        #[serde(default)]
        security: VmessSecurity,
    },
    Vless {
        uuid: String,
        /// `xtls-rprx-vision` 等。空串表示不启用 flow。
        #[serde(default)]
        flow: String,
        /// VLESS 的 `encryption` 字段，目前实际取值只有 `none`。
        #[serde(default = "encryption_none")]
        encryption: String,
    },
    Trojan {
        password: String,
    },
    Shadowsocks {
        method: String,
        password: String,
        /// UDP-over-TCP（2022 系列加密需要）。默认关闭以兼容老服务端。
        #[serde(default)]
        uot: bool,
    },
    Socks {
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    },
    Http {
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: String,
    },
}

fn encryption_none() -> String {
    "none".to_string()
}

impl Protocol {
    /// 协议名，用于 UI 展示与配置生成时的日志。
    pub fn name(&self) -> &'static str {
        match self {
            Self::Vmess { .. } => "vmess",
            Self::Vless { .. } => "vless",
            Self::Trojan { .. } => "trojan",
            Self::Shadowsocks { .. } => "shadowsocks",
            Self::Socks { .. } => "socks",
            Self::Http { .. } => "http",
        }
    }

    /// 参与 id 派生的凭据指纹（不含明文密码，避免泄漏到日志/文件名）。
    pub(crate) fn credential_fingerprint(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        match self {
            Self::Vmess { uuid, alter_id, security } => {
                uuid.hash(&mut h);
                alter_id.hash(&mut h);
                security.hash(&mut h);
            }
            Self::Vless { uuid, flow, encryption } => {
                uuid.hash(&mut h);
                flow.hash(&mut h);
                encryption.hash(&mut h);
            }
            Self::Trojan { password } => password.hash(&mut h),
            Self::Shadowsocks { method, password, uot } => {
                method.hash(&mut h);
                password.hash(&mut h);
                uot.hash(&mut h);
            }
            Self::Socks { username, password } | Self::Http { username, password } => {
                username.hash(&mut h);
                password.hash(&mut h);
            }
        }
        format!("{:016x}", h.finish())
    }
}

// ===========================================================================
// 传输层
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Tcp,
    WebSocket {
        #[serde(default = "slash")]
        path: String,
        #[serde(default)]
        host: String,
    },
    Grpc {
        #[serde(default)]
        service_name: String,
        #[serde(default)]
        multi_mode: bool,
    },
    HttpUpgrade {
        #[serde(default = "slash")]
        path: String,
        #[serde(default)]
        host: String,
    },
    /// XHTTP（Xray 的 `splithttp` 后继者）。
    Xhttp {
        #[serde(default = "slash")]
        path: String,
        #[serde(default)]
        host: String,
        #[serde(default = "auto_mode")]
        mode: String,
    },
    Quic {
        #[serde(default)]
        key: String,
        #[serde(default)]
        security: String,
    },
    Kcp {
        #[serde(default)]
        header_type: String,
        #[serde(default)]
        seed: String,
    },
    /// HTTP/2 伪装（Xray 的 `network: "http"`）。
    Http {
        #[serde(default)]
        host: String,
        #[serde(default = "slash")]
        path: String,
    },
}

fn slash() -> String {
    "/".into()
}

fn auto_mode() -> String {
    "auto".into()
}

impl Transport {
    pub fn network(&self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::WebSocket { .. } => "ws",
            Self::Grpc { .. } => "grpc",
            Self::HttpUpgrade { .. } => "httpupgrade",
            Self::Xhttp { .. } => "xhttp",
            Self::Quic { .. } => "quic",
            Self::Kcp { .. } => "kcp",
            Self::Http { .. } => "http",
        }
    }

    /// 该传输是否**必然**建立在 TLS/REALITY 之上（用于校验配置合法性）。
    pub fn requires_tls(&self) -> bool {
        matches!(self, Self::Quic { .. })
    }
}

// ===========================================================================
// TLS / REALITY
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RealitySettings {
    pub public_key: String,
    #[serde(default)]
    pub short_id: String,
    /// REALITY 的 `spiderX`，决定回落站点的爬取路径。
    #[serde(default = "slash")]
    pub spider_x: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TlsSettings {
    pub enabled: bool,
    /// SNI。留空时由 `xray::config` 回退到节点的 `address`。
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub allow_insecure: bool,
    #[serde(default)]
    pub alpn: Vec<String>,
    /// uTLS 指纹，如 `chrome` / `firefox` / `safari` / `randomized`。
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub reality: Option<RealitySettings>,
}

impl TlsSettings {
    /// 没有显式开启、但携带了 REALITY 参数时，视为已开启。
    pub fn effective_enabled(&self) -> bool {
        self.enabled || self.reality.is_some()
    }

    pub fn effective_security(&self) -> &'static str {
        if self.reality.is_some() {
            "reality"
        } else if self.enabled {
            "tls"
        } else {
            "none"
        }
    }
}

// ===========================================================================
// 多路复用
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MuxSettings {
    pub enabled: bool,
    #[serde(default = "default_mux_concurrency")]
    pub concurrency: u32,
    /// XUDP：把 UDP 也放进 mux 连接（Xray 扩展）。
    #[serde(default)]
    pub xudp_concurrency: u32,
}

fn default_mux_concurrency() -> u32 {
    8
}

// ===========================================================================
// 节点
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeSource {
    /// 来自订阅，`id` 是订阅 id。
    Subscription { id: String },
    /// 手工添加。
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub name: String,
    /// 域名或 IP，不带方括号（IPv6 由渲染层补括号）。
    pub address: String,
    pub port: u16,
    pub protocol: Protocol,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub tls: TlsSettings,
    #[serde(default)]
    pub mux: Option<MuxSettings>,
    pub source: NodeSource,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 原始分享链接，便于“复制链接”与排查解析问题。
    #[serde(default)]
    pub raw_uri: Option<String>,
}

impl Node {
    /// 从协议 + 端点派生稳定 id。
    ///
    /// 用 `DefaultHasher` 而不是加密哈希：这里只需要“稳定且低碰撞”，
    /// 不涉及安全属性，没必要为此引入 `sha2`。
    pub fn compute_id(protocol: &Protocol, address: &str, port: u16) -> NodeId {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        protocol.name().hash(&mut h);
        address.to_ascii_lowercase().hash(&mut h);
        port.hash(&mut h);
        protocol.credential_fingerprint().hash(&mut h);
        format!("n{:016x}", h.finish())
    }

    /// 重新派生 id（解析完成后调用）。
    pub fn refresh_id(&mut self) {
        self.id = Self::compute_id(&self.protocol, &self.address, self.port);
    }

    /// Xray outbound 的 tag。
    pub fn outbound_tag(&self) -> String {
        format!("node-{}", self.id)
    }

    /// `address:port`，IPv6 会自动加方括号。
    pub fn endpoint(&self) -> String {
        if self.address.contains(':') {
            format!("[{}]:{}", self.address, self.port)
        } else {
            format!("{}:{}", self.address, self.port)
        }
    }

    /// UI 上显示的“协议 / 传输 / 安全”摘要，例如 `vless + ws + tls`。
    pub fn summary(&self) -> String {
        let mut parts = vec![self.protocol.name().to_string(), self.transport.network().to_string()];
        let sec = self.tls.effective_security();
        if sec != "none" {
            parts.push(sec.to_string());
        }
        parts.join(" + ")
    }

    pub fn is_valid(&self) -> bool {
        !self.address.is_empty() && self.port != 0
    }
}

// ===========================================================================
// 订阅
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default = "default_update_interval")]
    pub update_interval_hours: u32,
    /// Unix 时间戳（秒）。
    #[serde(default)]
    pub last_updated: Option<u64>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub node_count: usize,
    /// 来自 HTTP 响应头 `subscription-userinfo`。
    #[serde(default)]
    pub usage: Option<SubscriptionUsage>,
}

fn yes() -> bool {
    true
}

fn default_update_interval() -> u32 {
    24
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubscriptionUsage {
    pub upload: u64,
    pub download: u64,
    pub total: u64,
    /// 到期时间（Unix 秒）。
    pub expire: Option<u64>,
}

impl SubscriptionUsage {
    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// 已用比例，`total == 0` 时返回 `None`（表示不限量）。
    pub fn ratio(&self) -> Option<f64> {
        if self.total == 0 {
            None
        } else {
            Some((self.used() as f64 / self.total as f64).clamp(0.0, 1.0))
        }
    }

    /// 解析 `upload=1; download=2; total=3; expire=4` 形式的响应头。
    pub fn parse_header(value: &str) -> Self {
        let mut out = Self::default();
        for part in value.split(';') {
            let Some((k, v)) = part.split_once('=') else { continue };
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "upload" => out.upload = v.parse().unwrap_or(0),
                "download" => out.download = v.parse().unwrap_or(0),
                "total" => out.total = v.parse().unwrap_or(0),
                "expire" => out.expire = v.parse().ok().filter(|x| *x > 0),
                _ => {}
            }
        }
        out
    }
}

// ===========================================================================
// 设置
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProxyMode {
    /// 完全不接管流量，核心可以停着。
    Direct,
    /// 只设置系统 HTTP/SOCKS 代理（`networksetup -setwebproxy` 等）。
    #[default]
    SystemProxy,
    /// TUN 全局接管。
    Tun,
}

impl ProxyMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::SystemProxy => "system_proxy",
            Self::Tun => "tun",
        }
    }

    /// 该模式是否必须安装特权 helper。
    pub fn needs_helper(&self) -> bool {
        matches!(self, Self::Tun)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoutingPreset {
    /// 全部流量走代理。
    GlobalProxy,
    /// 大陆域名/IP 直连，其余走代理。
    #[default]
    BypassMainland,
    /// 只有规则列表里的走代理，其余直连。
    WhitelistProxy,
    /// 全部直连（调试用）。
    DirectAll,
    /// 使用用户自定义规则。
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DnsHandling {
    /// DNS 查询**全部**交给远端解析（走代理）。
    ///
    /// 实测代价很大：每个查询要经节点往返约 **450ms**（本机实测
    /// 440–520ms），而国内 DNS 直连只要 **1ms**。更要命的是它把
    /// 「域名能不能解析」和「节点快不快」绑在了一起 —— 节点一忙或
    /// 一抖，查询就超过内核的 DNS 超时，日志里刷
    /// `Post "https://1.1.1.1/dns-query": context canceled`，
    /// 而用户看到的是「什么都打不开」。
    ///
    /// 仍然保留这个选项：它对「宁可慢也不要污染」的场景是合理的。
    Proxy,
    /// 国内域名本地直连解析，其余走代理。**[默认]**
    #[default]
    SplitByRule,
    /// 全部本地直连解析。
    Direct,
    /// 使用自定义服务器列表。
    Custom,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DnsSettings {
    #[serde(default)]
    pub mode: DnsHandling,
    /// 走代理的 DNS（支持 `https://` / `tls://` / 纯 IP）。
    #[serde(default = "default_remote_dns")]
    pub remote_servers: Vec<String>,
    /// 直连的 DNS。
    #[serde(default = "default_direct_dns")]
    pub direct_servers: Vec<String>,
    /// 静态 hosts，直接返回，不发起查询。
    #[serde(default)]
    pub hosts: Vec<(String, String)>,
    /// `UseIP` / `UseIPv4` / `UseIPv6`。
    #[serde(default = "default_query_strategy")]
    pub query_strategy: String,
    #[serde(default)]
    pub disable_cache: bool,
    /// 内核 DNS 查询是否也走嗅探后的域名（关闭时按 IP 查询）。
    #[serde(default = "yes")]
    pub sniffing: bool,
    /// 是否在启动时自动探测各解析器、把最快的排到前面。
    ///
    /// **两组分开测、分开写回**，因为它们在配置里的用法本来就不一样：
    ///
    /// * 国内组：明文 UDP 绑物理网卡**直连**测 → `direct_servers`；
    /// * 国外组：DoH **经节点**测 → `remote_servers`。
    ///
    /// 国外组的探测要求节点已经在跑（直连连不上国外 DoH）。没连节点时该组
    /// 标成「未探测」并保持原顺序，不会拿直连的超时冒充「这组都不通」。
    #[serde(default = "yes")]
    pub auto_select: bool,
}

fn default_remote_dns() -> Vec<String> {
    // 两个都用 **IP 形式**的 DoH 端点，刻意不用 `https://dns.google/dns-query`。
    //
    // 后者的主机名本身要先被解析一次，而解析它用的还是这套 DNS ——
    // 一个自举依赖。内核能处理，代价是多一次串行查询，而且失败时
    // 报错会指向一个和用户无关的域名。IP 端点没有这个问题。
    vec!["https://1.1.1.1/dns-query".into(), "https://8.8.8.8/dns-query".into()]
}

fn default_direct_dns() -> Vec<String> {
    vec!["223.5.5.5".into(), "119.29.29.29".into()]
}

fn default_query_strategy() -> String {
    "UseIP".into()
}

impl Default for DnsSettings {
    fn default() -> Self {
        Self {
            mode: DnsHandling::default(),
            remote_servers: default_remote_dns(),
            direct_servers: default_direct_dns(),
            hosts: Vec::new(),
            query_strategy: default_query_strategy(),
            disable_cache: false,
            sniffing: true,
            auto_select: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DatapathMode {
    /// Xray 原生 TUN 入站（`"protocol": "tun"`，内置 gVisor 协议栈）。
    ///
    /// 要求 **Xray-core >= v26.1.18**（该版本首次包含 `tun_darwin.go`）。
    /// 这是当前推荐路径：不需要 tun2socks 旁路进程，UDP/QUIC 支持更完整。
    #[default]
    XrayNativeTun,
    /// 外部 tun2socks 类数据面。
    ///
    /// 仅在两种情况下需要：一是用户的 Xray 版本太老，二是需要
    /// tun2socks 特有的能力（例如某些 UDP 中继策略）。
    ExternalTun2Socks,
    /// helper 只建 utun 并把 fd 交给 GUI，数据面由 GUI 自己实现。
    ///
    /// 用于自研用户态协议栈的实验路径。
    HandoffFdOnly,
}

/// 谁持有 utun 的 fd，也就是**数据面以什么权限运行**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FdOwnership {
    /// helper 建好 utun 后通过 `SCM_RIGHTS` 把 fd 交给调用方，
    /// 数据面以**普通用户**身份运行。安全性最好，是默认选项。
    #[default]
    HandoffToCaller,
    /// helper 自己持有 fd 并拉起数据面进程，数据面以 **root** 运行。
    ///
    /// 兼容性最好（例如外部 tun2socks 只接受接口名、不接受 fd），
    /// 但把整个代理内核暴露在 root 权限下。
    HelperHolds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunSettings {
    #[serde(default = "default_tun_mtu")]
    pub mtu: u16,
    /// 隧道内地址（CIDR 字符串）。
    #[serde(default = "default_tun_network")]
    pub network: String,
    /// 哨兵 DNS：写入系统的“假”解析器，保证 53 端口流量必然进隧道。
    #[serde(default = "default_sentinel_dns")]
    pub sentinel_dns: String,
    #[serde(default)]
    pub ipv6: xt_proto::Ipv6Mode,
    /// 是否把 RFC1918 / 链路本地 / 多播排除在隧道之外。
    #[serde(default = "yes")]
    pub bypass_private: bool,
    #[serde(default)]
    pub datapath: DatapathMode,
    #[serde(default)]
    pub fd_ownership: FdOwnership,
    /// 传给 Xray 原生 tun 入站的 `autoOutboundsInterface`。
    ///
    /// 设为物理接口名（如 `en0`）后，Xray 会把**出站 socket 绑定到该接口**，
    /// 从而让「连代理服务器」的流量天然绕开隧道 —— 这是比手工加 host 路由
    /// 更干净的防环手段（`sockopt.interface` 在 macOS 上走 `IP_BOUND_IF`）。
    #[serde(default)]
    pub bind_outbound_to: Option<String>,
}

fn default_tun_mtu() -> u16 {
    xt_proto::DEFAULT_MTU
}

fn default_tun_network() -> String {
    xt_proto::DEFAULT_TUN_NETWORK_V4.into()
}

fn default_sentinel_dns() -> String {
    xt_proto::DEFAULT_SENTINEL_DNS_V4.into()
}

impl Default for TunSettings {
    fn default() -> Self {
        Self {
            mtu: default_tun_mtu(),
            network: default_tun_network(),
            sentinel_dns: default_sentinel_dns(),
            ipv6: xt_proto::Ipv6Mode::default(),
            bypass_private: true,
            datapath: DatapathMode::default(),
            fd_ownership: FdOwnership::default(),
            bind_outbound_to: None,
        }
    }
}

impl TunSettings {
    pub fn network_cidr(&self) -> Option<Cidr> {
        self.network.parse().ok()
    }

    /// 该数据面模式是否要求 Xray >= 26.1.18。
    pub fn requires_modern_core(&self) -> bool {
        matches!(self.datapath, DatapathMode::XrayNativeTun)
    }
}

/// Fake-IP 设置。
///
/// 很多人以为 Fake-IP 是 sing-box 独有 —— **不是**。Xray 有原生的
/// `fakedns` 配置段，配合入站的
/// `sniffing.destOverride: ["fakedns+others"]` 使用，语义与 sing-box 的
/// fakeip + sniffing 等价。
///
/// 它解决的核心问题是：客户端在拿到域名解析结果之前就把连接发出来了，
/// 此时只能看到 IP，无法按域名分流。Fake-IP 让 DNS 立刻返回一个假地址，
/// 内核再把这个假地址还原成域名，于是域名分流对「按 IP 发起连接」的程序
/// 也能生效。
///
/// **代价（必须在 UI 里向用户说明）**：Fake-IP 会污染本机 DNS 缓存，
/// 隧道关闭后的一段时间内可能出现「网络无法访问」，需要等缓存过期或手动
/// 刷新。所以默认关闭，由用户按需开启。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeDnsSettings {
    #[serde(default)]
    pub enabled: bool,
    /// 假地址池。必须是不会被真实路由的网段。
    #[serde(default = "default_fake_ip_pool")]
    pub ip_pool: String,
    #[serde(default = "default_fake_ip_pool_size")]
    pub pool_size: u32,
}

fn default_fake_ip_pool() -> String {
    "198.18.0.0/16".into()
}

fn default_fake_ip_pool_size() -> u32 {
    65535
}

impl Default for FakeDnsSettings {
    fn default() -> Self {
        Self { enabled: false, ip_pool: default_fake_ip_pool(), pool_size: default_fake_ip_pool_size() }
    }
}

// ---------------------------------------------------------------------------
// 意图过滤（Jev 判定）
// ---------------------------------------------------------------------------
//
// 完整设计见 `docs/design/INTENT-FILTER.md`，判定引擎在 `xt-intent`。
// 这里只放**设置形状**：`xt-core` 不能依赖 `xt-intent`（会成环），
// 所以阈值在这里再写一份同名同义的结构，由桌面层转成 `xt_intent::Thresholds`，
// 并有一条单测钉住"逐字段都搬过去了"。

/// Jev 网关预设。URL 都写在这里，避免用户在界面上手抄地址抄错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntentPreset {
    /// TypeSafe 官方网关：`api.typesafe.ai`，模型 `jev-latest`，**需要自己的 Key**。
    #[default]
    Typesafe,
    /// OpenCode Zen：`opencode.ai/zen`，模型 `jev-1.13-free`，**免密钥**。
    ///
    /// 刻意单独列出来：它是"先试试看"的唯一零成本入口，但数据会经过第三方网关，
    /// 所以界面上必须写清楚，不能当成默认值偷偷用。
    Zen,
    OpenRouter,
    Vercel,
    /// 自填 base URL（高级用户 / 自建网关）。
    Custom,
}

impl IntentPreset {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Typesafe => "typesafe",
            Self::Zen => "zen",
            Self::OpenRouter => "openrouter",
            Self::Vercel => "vercel",
            Self::Custom => "custom",
        }
    }

    /// 该预设的 base URL（`Custom` 返回 `None`，由用户填）。
    pub fn base_url(self) -> Option<&'static str> {
        match self {
            Self::Typesafe => Some("https://api.typesafe.ai"),
            Self::Zen => Some("https://opencode.ai/zen"),
            Self::OpenRouter => Some("https://openrouter.ai/api"),
            Self::Vercel => Some("https://ai-gateway.vercel.sh"),
            Self::Custom => None,
        }
    }

    /// 该预设的默认模型。
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Typesafe => "jev-latest",
            Self::Zen => "jev-1.13-free",
            Self::OpenRouter => "typesafe/jev-1.13",
            Self::Vercel => "typesafe-ai/jev",
            Self::Custom => "jev-latest",
        }
    }

    /// 是否需要 API Key。**只有 Zen 免密钥**（上游客户端的硬约束）。
    pub fn needs_key(self) -> bool {
        !matches!(self, Self::Zen)
    }
}

/// 可以定罪的端点类别（`xt_intent::Category` 的镜像）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentCategory {
    AdOrMonetization,
    TrackerOrAnalytics,
    CdnOrInfra,
    ApiOrService,
    HumanSite,
    Unknown,
}

impl IntentCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AdOrMonetization => "ad_or_monetization",
            Self::TrackerOrAnalytics => "tracker_or_analytics",
            Self::CdnOrInfra => "cdn_or_infra",
            Self::ApiOrService => "api_or_service",
            Self::HumanSite => "human_site",
            Self::Unknown => "unknown",
        }
    }
}

/// 闸门阈值（`xt_intent::Thresholds` 的镜像；默认值必须一致，有测试钉住）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentThresholds {
    #[serde(default = "intent_ads_min")]
    pub ads_intent_min: f32,
    #[serde(default = "intent_choice_min")]
    pub choice_confidence_min: f32,
    #[serde(default = "intent_risk_max")]
    pub risk_of_breakage_max: f32,
    /// 形状加分最多能把阈值压低多少。
    #[serde(default = "intent_bonus_max")]
    pub shape_bonus_max: f32,
    #[serde(default = "intent_block_categories")]
    pub block_categories: Vec<IntentCategory>,
}

fn intent_ads_min() -> f32 {
    0.85
}
fn intent_choice_min() -> f32 {
    0.5
}
fn intent_risk_max() -> f32 {
    0.3
}
fn intent_bonus_max() -> f32 {
    0.10
}
fn intent_block_categories() -> Vec<IntentCategory> {
    vec![IntentCategory::AdOrMonetization, IntentCategory::TrackerOrAnalytics]
}

impl Default for IntentThresholds {
    fn default() -> Self {
        Self {
            ads_intent_min: intent_ads_min(),
            choice_confidence_min: intent_choice_min(),
            risk_of_breakage_max: intent_risk_max(),
            shape_bonus_max: intent_bonus_max(),
            block_categories: intent_block_categories(),
        }
    }
}

impl IntentThresholds {
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        for (name, v) in [
            ("ads_intent_min", self.ads_intent_min),
            ("choice_confidence_min", self.choice_confidence_min),
            ("risk_of_breakage_max", self.risk_of_breakage_max),
            ("shape_bonus_max", self.shape_bonus_max),
        ] {
            if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                errs.push(format!("意图阈值 {name} 必须在 0..=1，现在是 {v}"));
            }
        }
        if self.block_categories.is_empty() {
            errs.push("意图阈值 block_categories 不能为空（空等于永不拦截）".into());
        }
        errs
    }
}

/// 用户对"误杀"的纠正动作。**必须由用户显式选**，没有默认值可用而不告知。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IntentAllowAction {
    #[default]
    Direct,
    Proxy,
}

/// 一条放行纠正。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentAllowOverride {
    pub host: String,
    #[serde(default)]
    pub action: IntentAllowAction,
}

/// 意图过滤的全部设置。**默认关闭 + 演练模式**。
///
/// 两个默认值都是产品态度：
/// * 关闭：判定会把"你访问过哪些域名"发到远端网关，不默认替用户做这个决定；
/// * 演练模式开：即使开了功能，也先只记录"本该拦谁"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentSettings {
    #[serde(default)]
    pub enabled: bool,
    /// 演练模式：判决照做、审计照写，但**不生成 block 规则**。
    #[serde(default = "yes")]
    pub drill: bool,
    #[serde(default)]
    pub preset: IntentPreset,
    /// 预设为 `Custom` 时的 base URL；其它预设忽略。
    #[serde(default)]
    pub custom_base_url: String,
    /// 模型 id。留空表示用预设的默认模型。
    #[serde(default)]
    pub model: String,
    /// Jev API Key 的**引用**，不是明文。
    ///
    /// 约定与订阅 token 一致（见 `store.rs` 模块文档）：密钥进 macOS Keychain，
    /// `settings.json` 里只留 `keychain:<service>/<account>`；空字符串表示没配。
    /// 桌面层负责读写 Keychain —— **这个字段里永远不该出现明文密钥**。
    #[serde(default)]
    pub api_key_ref: String,
    #[serde(default)]
    pub thresholds: IntentThresholds,
    #[serde(default = "intent_per_minute")]
    pub per_minute: u32,
    #[serde(default = "intent_per_day")]
    pub per_day: u32,
    #[serde(default = "intent_cache_entries")]
    pub cache_max_entries: usize,
    /// 永不判定的域名（用户白名单）。
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// 用户对误杀的纠正。
    #[serde(default)]
    pub allow_overrides: Vec<IntentAllowOverride>,
    /// 是否把发给网关的上下文也写进审计（默认否，见设计 §9）。
    #[serde(default)]
    pub store_context_in_audit: bool,
}

fn intent_per_minute() -> u32 {
    10
}
fn intent_per_day() -> u32 {
    200
}
fn intent_cache_entries() -> usize {
    5000
}

impl Default for IntentSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            drill: true,
            preset: IntentPreset::default(),
            custom_base_url: String::new(),
            model: String::new(),
            api_key_ref: String::new(),
            thresholds: IntentThresholds::default(),
            per_minute: intent_per_minute(),
            per_day: intent_per_day(),
            cache_max_entries: intent_cache_entries(),
            allow_hosts: Vec::new(),
            allow_overrides: Vec::new(),
            store_context_in_audit: false,
        }
    }
}

impl IntentSettings {
    /// Keychain 里存 Jev Key 的服务名/账号（桌面层读写时用同一处定义）。
    pub const KEYCHAIN_ACCOUNT: &'static str = "jev-api-key";

    /// 生效的 base URL。
    pub fn base_url(&self) -> Option<&str> {
        match self.preset {
            IntentPreset::Custom => {
                let t = self.custom_base_url.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            }
            other => other.base_url(),
        }
    }

    /// 生效的模型 id。
    pub fn model(&self) -> &str {
        let m = self.model.trim();
        if m.is_empty() {
            self.preset.default_model()
        } else {
            m
        }
    }

    /// 是否已经具备"能问一次"的最小条件（网关 + 模型 + 密钥）。
    pub fn ready(&self) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(url) = self.base_url() else { return false };
        if !url.starts_with("https://") {
            return false;
        }
        if self.preset.needs_key() && self.api_key_ref.trim().is_empty() {
            return false;
        }
        true
    }

    /// 保存设置前调它。返回**全部**问题，不是第一个。
    pub fn validate(&self) -> Vec<String> {
        let mut errs = self.thresholds.validate();
        if self.enabled {
            match self.base_url() {
                None => errs.push("意图过滤已开启，但没有可用的网关地址".into()),
                Some(url) if !url.starts_with("https://") => {
                    errs.push("意图过滤的网关地址必须是 https://".into())
                }
                Some(_) => {}
            }
            if self.preset.needs_key() && self.api_key_ref.trim().is_empty() {
                errs.push(format!(
                    "预设「{}」需要 Jev API Key；想零密钥试水请改用 Zen 预设",
                    self.preset.as_str()
                ));
            }
        }
        if self.cache_max_entries == 0 {
            errs.push("意图判决缓存条数不能为 0".into());
        }
        if self.per_day > 0 && self.per_minute > self.per_day {
            errs.push("意图过滤的每分钟上限不能大于每天上限".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for o in &self.allow_overrides {
            let h = o.host.trim().to_ascii_lowercase();
            if h.is_empty() {
                errs.push("放行纠正里有空域名".into());
            } else if !seen.insert(h) {
                errs.push("放行纠正里有重复域名".into());
            }
        }
        errs
    }
}

/// MITM（内容级判定）的设置。
///
/// # 默认全关，而且**空名单 = 不出规则**
///
/// 这是整个功能里唯一会改系统状态的部分（要往系统钥匙串装一个本地根证书、
/// 还要拆 TLS），所以默认关闭；而且即使打开了，**名单为空时也不生成任何 steer 规则** ——
/// 「打开了但什么都没配」在行为上应当等于没开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MitmSettings {
    #[serde(default)]
    pub enabled: bool,
    /// MITM 进程监听的本地端口（被 steer 的流量会送到这里）。
    #[serde(default = "default_mitm_port")]
    pub listen_port: u16,
    /// MITM **回连**用的本地 socks 入站端口。
    ///
    /// **必须与 tun 的入站分开**：steer 规则里的 `inboundTag` 只列 tun/socks/http，
    /// 于是回连天然不会命中 steer 规则 —— 防自环是靠**构造**，不是靠一条"旁路规则"
    /// （后者容易被后来者调整顺序时改坏）。
    #[serde(default = "default_mitm_upstream_port")]
    pub upstream_port: u16,
    /// 只对**这些域名**拆 TLS（opt-in）。空 = 等同于没开。
    #[serde(default)]
    pub domains: Vec<String>,
    /// 对 opt-in 域名拦掉 UDP/443，逼浏览器回退 TCP（QUIC 拆不了）。
    #[serde(default)]
    pub block_quic: bool,
    /// **可选**的响应体裁剪（设计 §8.5 那一行）。
    ///
    /// 默认 `None` = 不碰任何响应体。它不是"顺手多做的优化"，而是**语义改动**：
    /// 我们要从别人的响应里删掉条目，所以必须由用户单独点开，而且只支持
    /// 一种窄口径动作（见 [`MitmBodyStrip`]）。
    #[serde(default)]
    pub body_strip: Option<MitmBodyStrip>,
}

/// 响应体裁剪的**唯一**支持口径：删掉 JSON 里某个数组内、某个布尔字段为 `true` 的元素。
///
/// # 为什么只有这一种
///
/// 设计里明确"不做 HTML DOM 重写、不注入脚本、不改响应语义" —— 那等于在里面
/// 再写一个 AdGuard，风险与工程量都不成比例。而"删掉一个 JSON 数组元素"能覆盖
/// 同一站点接口内的推广条目，且**副作用可穷举**：
/// 我们只把某些元素摘出去，其余字节由 `serde_json` 原样重写。
///
/// 代价要如实写：**重写会改变响应体字节**（键顺序、空白都可能不同），
/// 所以只对用户点过名的域名生效，且失败一律放行（见 `xt-mitm` 的 `DeclineReason`）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MitmBodyStrip {
    /// 要裁剪的数组，RFC 6901 JSON 指针（例如 `/data/items`；`""` = 根数组）。
    #[serde(default)]
    pub pointer: String,
    /// 数组元素里必须为布尔 `true` 才删除的字段名（例如 `promoted`）。
    #[serde(default)]
    pub field: String,
}

impl MitmBodyStrip {
    /// 两项都填了才算配置好。空字符串是"没配"，不是"配了空指针"。
    pub fn is_configured(&self) -> bool {
        !self.pointer.is_empty() && !self.field.is_empty()
    }
}

fn default_mitm_port() -> u16 {
    10810
}
fn default_mitm_upstream_port() -> u16 {
    10811
}

impl Default for MitmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_port: default_mitm_port(),
            upstream_port: default_mitm_upstream_port(),
            domains: Vec::new(),
            block_quic: false,
            body_strip: None,
        }
    }
}

impl MitmSettings {
    /// 出站/入站/规则要不要生成 —— 由它一处决定，避免三处各判一遍。
    pub fn is_active(&self) -> bool {
        self.enabled && !self.domains.is_empty()
    }

    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.enabled && self.domains.is_empty() {
            errs.push("MITM 已开启，但 opt-in 域名列表是空的（等于没开）".into());
        }
        if !self.enabled {
            return errs;
        }
        if self.listen_port == 0 || self.upstream_port == 0 {
            errs.push("MITM 的监听/回连端口不能为 0".into());
        }
        if self.listen_port == self.upstream_port {
            errs.push("MITM 的监听端口与回连端口不能相同（会自己连自己）".into());
        }
        if let Some(strip) = &self.body_strip {
            if strip.pointer.is_empty() && !strip.field.is_empty() {
                errs.push(
                    "响应体裁剪填了字段名但没有 JSON 指针：请补上指针（根数组要显式写指针），\
                     或把整项清空"
                        .into(),
                );
            }
            if strip.field.is_empty() && !strip.pointer.is_empty() {
                errs.push("响应体裁剪填了 JSON 指针但没有字段名（不知道要删哪些元素）".into());
            }
            if strip.field.contains('/') || strip.field.contains(' ') {
                errs.push(format!(
                    "响应体裁剪的字段名 {:?} 不像一个 JSON 键（不能含空格或斜杠）",
                    strip.field
                ));
            }
            if !strip.pointer.is_empty() && !strip.pointer.starts_with('/') {
                errs.push(format!(
                    "响应体裁剪的 JSON 指针 {:?} 必须以 '/' 开头（RFC 6901）",
                    strip.pointer
                ));
            }
        }
        errs
    }
}

/// 设置文件的版本号。用来做**一次性迁移**。
///
/// 0 表示 0.1.0 时代写下的文件（那时还没有这个字段）。
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppSettings {
    /// 见 [`SETTINGS_VERSION`]。缺失时按 0 处理，走迁移。
    #[serde(default)]
    pub settings_version: u32,
    #[serde(default)]
    pub mode: ProxyMode,
    #[serde(default = "default_socks_port")]
    pub socks_port: u16,
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// 允许局域网设备使用本机代理。
    #[serde(default)]
    pub allow_lan: bool,
    /// 当前选中的节点。`None` 表示尚未选择。
    #[serde(default)]
    pub selected_node: Option<NodeId>,
    #[serde(default)]
    pub routing_preset: RoutingPreset,
    #[serde(default)]
    pub custom_rules: Vec<crate::routing::RoutingRule>,
    #[serde(default)]
    pub tun: TunSettings,
    #[serde(default)]
    pub dns: DnsSettings,
    #[serde(default)]
    pub fakedns: FakeDnsSettings,
    /// 覆盖内置核心路径（开发期常用）。
    #[serde(default)]
    pub core_path: Option<PathBuf>,
    #[serde(default)]
    pub launch_at_login: bool,
    /// `silent` / `error` / `warning` / `info` / `debug`。
    /// **重连意图**：上次退出时这条隧道是开着的吗。
    ///
    /// 自更新会先退出 app（核心随之关闭）、替换、再重启 —— 如果不记住这件事，
    /// 重启之后就是「网断了」，而用户并没有关过它。用户主动点「停止」时
    /// 这里会被清掉，所以「除非我关闭，否则不该断」是成立的。
    #[serde(default)]
    pub was_connected: bool,
    /// 启动时如果上次是连着的，要不要自动连回去。
    #[serde(default = "yes")]
    pub auto_reconnect: bool,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// 是否在切换模式时自动清理系统代理设置。
    #[serde(default = "yes")]
    pub restore_system_proxy_on_exit: bool,
    /// 是否把实时网速显示在窗口标题栏与菜单栏。
    ///
    /// 默认开：关着窗口时菜单栏的读数就是这个 App 最有用的信息。
    /// 但菜单栏空间是公共资源，所以要给一个关掉它的开关。
    #[serde(default = "yes")]
    pub show_speed_in_title: bool,
    /// 意图过滤（Jev 判定）。默认关闭 + 演练模式，见 [`IntentSettings`]。
    ///
    /// `#[serde(default)]` 让老 `settings.json` 直接可用，**不需要迁移**：
    /// 新字段缺省就等于"用户从没开过这个功能"，与迁移的语义一致。
    #[serde(default)]
    pub intent: IntentSettings,
    /// MITM（内容级判定）。默认关闭；空名单等于没开。见 [`MitmSettings`]。
    #[serde(default)]
    pub mitm: MitmSettings,
}

fn default_socks_port() -> u16 {
    10808
}

fn default_http_port() -> u16 {
    10809
}

fn default_log_level() -> String {
    "warning".into()
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            settings_version: SETTINGS_VERSION,
            mode: ProxyMode::default(),
            socks_port: default_socks_port(),
            http_port: default_http_port(),
            allow_lan: false,
            selected_node: None,
            routing_preset: RoutingPreset::default(),
            custom_rules: Vec::new(),
            tun: TunSettings::default(),
            dns: DnsSettings::default(),
            fakedns: FakeDnsSettings::default(),
            core_path: None,
            launch_at_login: false,
            was_connected: false,
            auto_reconnect: true,
            log_level: default_log_level(),
            restore_system_proxy_on_exit: true,
            show_speed_in_title: true,
            intent: IntentSettings::default(),
            mitm: MitmSettings::default(),
        }
    }
}

impl AppSettings {
    /// 把旧版本的设置升级到当前版本，返回**人类可读的改动列表**（没有改动就是空）。
    ///
    /// 返回列表而不是只打日志：迁移是替用户做决定，必须让他在界面上
    /// 看得到「什么被改了」，否则就成了暗中改配置。
    pub fn migrate(&mut self) -> Vec<String> {
        let mut changes = Vec::new();

        if self.settings_version < 1 {
            // 0.1.0 的默认 DNS 模式是 `proxy`（全部走远端解析）。实测每个
            // 查询要 450ms 上下，而且节点一抖就整片解析失败 —— 日志里刷
            // `context canceled`，用户看到的是「上不了网」。
            //
            // 这里只改**默认值留下的痕迹**，改不动用户的显式选择是没法区分的
            // （JSON 里看不出哪个值是默认填的），所以统一迁移，并在界面上说明。
            if self.dns.mode == DnsHandling::Proxy {
                self.dns.mode = DnsHandling::SplitByRule;
                changes.push("DNS 改为「按规则分流」（原来全部走代理解析）".into());
            }
            // 域名形式的 DoH 端点要先解析自己，属于自举依赖。
            if let Some(pos) = self
                .dns
                .remote_servers
                .iter()
                .position(|s| s.contains("dns.google"))
            {
                self.dns.remote_servers[pos] = "https://8.8.8.8/dns-query".into();
                changes.push("远端 DNS 去掉需要自举解析的 dns.google，换成 8.8.8.8".into());
            }
            self.settings_version = 1;
        }

        changes
    }

    /// 做一次边界校验，避免把非法值送进核心或 helper。
    pub fn validate(&self) -> Result<(), crate::Error> {
        for (name, port) in [("socks_port", self.socks_port), ("http_port", self.http_port)] {
            if port < 1024 {
                return Err(crate::Error::InvalidConfig(format!(
                    "{name} = {port} 属于特权端口，请使用 1024 以上"
                )));
            }
        }
        if self.socks_port == self.http_port {
            return Err(crate::Error::InvalidConfig("socks_port 与 http_port 不能相同".into()));
        }
        self.tun
            .network_cidr()
            .ok_or_else(|| crate::Error::InvalidConfig(format!("非法 TUN 网段: {}", self.tun.network)))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_node() -> Node {
        Node {
            id: String::new(),
            name: "东京 01".into(),
            address: "jp1.example.com".into(),
            port: 443,
            protocol: Protocol::Vless {
                uuid: "b831381d-6324-4d53-ad4f-8cda48b30811".into(),
                flow: "xtls-rprx-vision".into(),
                encryption: "none".into(),
            },
            transport: Transport::WebSocket { path: "/ws".into(), host: "jp1.example.com".into() },
            tls: TlsSettings { enabled: true, server_name: "jp1.example.com".into(), ..Default::default() },
            mux: None,
            source: NodeSource::Manual,
            tags: vec![],
            raw_uri: None,
        }
    }

    #[test]
    fn node_id_is_stable_across_renames() {
        let mut a = sample_node();
        a.refresh_id();
        let mut b = sample_node();
        b.name = "改了名字".into();
        b.refresh_id();
        assert_eq!(a.id, b.id, "id 不应受显示名影响");
    }

    #[test]
    fn node_id_changes_with_endpoint() {
        let mut a = sample_node();
        a.refresh_id();
        let mut b = sample_node();
        b.port = 8443;
        b.refresh_id();
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn endpoint_brackets_ipv6() {
        let mut n = sample_node();
        n.address = "2001:db8::1".into();
        assert_eq!(n.endpoint(), "[2001:db8::1]:443");
    }

    #[test]
    fn subscription_usage_header_parses() {
        let u = SubscriptionUsage::parse_header("upload=100; download=200; total=1000; expire=1735689600");
        assert_eq!(u.used(), 300);
        assert_eq!(u.ratio(), Some(0.3));
        assert_eq!(u.expire, Some(1735689600));

        let unlimited = SubscriptionUsage::parse_header("upload=1; download=2; total=0");
        assert_eq!(unlimited.ratio(), None);
    }

    // -----------------------------------------------------------------------
    // 意图过滤的设置（判定引擎在 `xt-intent`，这里只钉住形状与默认值）
    // -----------------------------------------------------------------------

    /// 默认值本身就是产品态度：**不默认把用户的浏览域名发到远端网关**，
    /// 而且即使开了也先只记录（演练模式）。
    #[test]
    fn intent_is_off_and_in_drill_mode_by_default() {
        let i = IntentSettings::default();
        assert!(!i.enabled);
        assert!(i.drill, "演练模式必须默认开");
        assert!(!i.ready(), "没开就不该 ready");
        assert!(i.base_url().is_some(), "默认预设必须自带一个 https 网关");
        assert!(!i.model().is_empty());
    }

    /// 这四个数是**闸门阈值**，与 `xt_intent::Thresholds::default()` 必须一致。
    /// `xt-core` 不能依赖 `xt-intent`（会成环），所以这里显式钉住数值，
    /// 桌面层另有一条转换测试做交叉校验。
    #[test]
    fn intent_threshold_mirror_matches_the_engine_defaults() {
        let t = IntentThresholds::default();
        assert_eq!(t.ads_intent_min, 0.85);
        assert_eq!(t.choice_confidence_min, 0.5);
        assert_eq!(t.risk_of_breakage_max, 0.3);
        assert_eq!(t.shape_bonus_max, 0.10);
        assert_eq!(
            t.block_categories,
            vec![IntentCategory::AdOrMonetization, IntentCategory::TrackerOrAnalytics]
        );
    }

    #[test]
    fn only_zen_is_keyless() {
        for p in [IntentPreset::Typesafe, IntentPreset::OpenRouter, IntentPreset::Vercel] {
            assert!(p.needs_key(), "{:?} 需要密钥", p);
            assert!(p.base_url().unwrap().starts_with("https://"));
        }
        assert!(!IntentPreset::Zen.needs_key());
        assert_eq!(IntentPreset::Zen.default_model(), "jev-1.13-free");
        assert!(IntentPreset::Custom.base_url().is_none());
    }

    #[test]
    fn ready_requires_gateway_and_key_but_not_for_zen() {
        let mut i = IntentSettings { enabled: true, ..Default::default() };
        assert!(!i.ready(), "TypeSafe 预设没配 Key 之前不该 ready");
        i.api_key_ref = "keychain:com.xraytun.intent/jev-api-key".into();
        assert!(i.ready());

        let zen = IntentSettings { enabled: true, preset: IntentPreset::Zen, ..Default::default() };
        assert!(zen.ready(), "Zen 免密钥档应当直接可用");

        // 自定义网关必须自己填地址，而且必须是 https。
        let mut custom = IntentSettings { enabled: true, preset: IntentPreset::Custom, ..Default::default() };
        assert!(!custom.ready());
        custom.custom_base_url = "http://insecure.example".into();
        assert!(!custom.ready(), "明文网关必须拒绝");
        custom.custom_base_url = "https://gw.example".into();
        assert!(!custom.ready(), "自建网关同样需要密钥（只有 Zen 是免密钥档）");
        custom.api_key_ref = "keychain:com.xraytun.intent/jev-api-key".into();
        assert!(custom.ready());
    }

    #[test]
    fn validate_reports_every_problem_at_once() {
        let mut i = IntentSettings { enabled: true, ..Default::default() };
        i.thresholds.ads_intent_min = 1.7;
        i.thresholds.block_categories.clear();
        i.cache_max_entries = 0;
        i.per_minute = 500;
        i.per_day = 100;
        i.allow_overrides = vec![
            IntentAllowOverride { host: "a.example".into(), action: IntentAllowAction::Direct },
            IntentAllowOverride { host: "A.example ".into(), action: IntentAllowAction::Proxy },
            IntentAllowOverride { host: "  ".into(), action: IntentAllowAction::Direct },
        ];
        let errs = i.validate();
        // 阈值越界 ×1 + 类别空 ×1 + 缺 Key ×1 + 缓存 0 ×1 + 每分钟>每天 ×1 + 重复域名 ×1 + 空域名 ×1
        assert_eq!(errs.len(), 7, "{errs:?}");

        // 关闭状态下只校验"设置本身是否合法"，不抱怨缺 Key。
        let mut off = IntentSettings { enabled: false, ..Default::default() };
        off.thresholds.ads_intent_min = 1.7;
        assert_eq!(off.validate().len(), 1);
        assert!(IntentSettings::default().validate().is_empty());
    }

    /// 老 `settings.json`（没有 `intent` 字段）必须直接可用，**不需要迁移**。
    #[test]
    fn settings_written_before_this_feature_still_load() {
        let old = r#"{
            "settings_version": 1,
            "mode": "tun",
            "socks_port": 10808,
            "http_port": 10809,
            "selected_node": null,
            "routing_preset": "bypass_mainland",
            "custom_rules": [],
            "tun": {},
            "dns": {},
            "fakedns": {},
            "core_path": null,
            "launch_at_login": false,
            "was_connected": false,
            "auto_reconnect": true,
            "log_level": "warning",
            "restore_system_proxy_on_exit": true,
            "show_speed_in_title": true
        }"#;
        let s: AppSettings = serde_json::from_str(old).expect("老设置必须能读");
        assert!(!s.intent.enabled, "缺省即关闭");
        assert!(s.intent.drill);
        assert_eq!(s.intent.per_day, 200);
    }

    #[test]
    fn intent_settings_round_trip_through_json() {
        let mut s = AppSettings::default();
        s.intent.enabled = true;
        s.intent.preset = IntentPreset::Custom;
        s.intent.custom_base_url = "https://gw.example".into();
        s.intent.thresholds.ads_intent_min = 0.9;
        s.intent.allow_overrides = vec![IntentAllowOverride {
            host: "ok.example".into(),
            action: IntentAllowAction::Proxy,
        }];
        let json = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    /// 迁移必须把 0.1.0 的默认 DNS 模式改掉 —— 那正是「DNS 频繁
    /// context canceled」的根因（每个查询经节点约 450ms）。
    #[test]
    fn migrate_moves_proxy_dns_to_split_by_rule() {
        let mut s = AppSettings { settings_version: 0, ..Default::default() };
        s.dns.mode = DnsHandling::Proxy;
        s.dns.remote_servers = vec![
            "https://1.1.1.1/dns-query".into(),
            "https://dns.google/dns-query".into(),
        ];

        let changes = s.migrate();
        assert_eq!(s.dns.mode, DnsHandling::SplitByRule);
        assert!(
            !s.dns.remote_servers.iter().any(|x| x.contains("dns.google")),
            "需要自举解析的 DoH 端点必须被换掉"
        );
        assert_eq!(s.settings_version, SETTINGS_VERSION);
        assert_eq!(changes.len(), 2, "两处改动都要报告给用户：{changes:?}");
    }

    /// 迁移是幂等的：已经是当前的设置不该被动。
    #[test]
    fn migrate_is_idempotent_and_respects_new_files() {
        let mut s = AppSettings::default();
        assert_eq!(s.dns.mode, DnsHandling::SplitByRule, "新装的默认值就是分流");
        assert!(s.migrate().is_empty(), "当前版本的文件不该产生任何改动");
        assert!(s.migrate().is_empty(), "再跑一次仍然什么都不做");
    }

    /// 用户显式选了「全部直连 DNS」时，迁移不该把它改成按规则分流。
    #[test]
    fn migrate_leaves_explicit_direct_dns_alone() {
        let mut s = AppSettings { settings_version: 0, ..Default::default() };
        s.dns.mode = DnsHandling::Direct;
        let changes = s.migrate();
        assert_eq!(s.dns.mode, DnsHandling::Direct);
        assert!(changes.is_empty(), "只迁移旧的默认值，不动别的选择");
    }

    #[test]
    fn settings_reject_privileged_ports() {
        let mut s = AppSettings {
            socks_port: 80,
            ..Default::default()
        };
        assert!(s.validate().is_err());
        s.socks_port = 10808;
        assert!(s.validate().is_ok());
    }
}
