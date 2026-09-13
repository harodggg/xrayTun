//! `xt-proto` —— 非特权 GUI 进程与特权 helper 守护进程之间的线协议。
//!
//! 设计约束（见 `docs/06-helper-protocol.md`）：
//!
//! 1. **helper 不做任何“应用层”决策**。它只接受有限、可校验的网络配置变更指令
//!    （建 utun、加路由、设 DNS、拉起数据面），绝不解码代理协议、绝不读用户订阅。
//!    这样即使 GUI 被攻破，攻击面也只是“本机网络配置”，而不是“以 root 身份执行任意代码”。
//! 2. **所有请求都要显式版本化**，helper 会拒绝协议不匹配的客户端。
//! 3. **幂等 + 会话化**：每次 `TunUp` 携带 `session_id`，helper 记录该会话安装的
//!    全部副作用（路由、DNS、子进程），`TunDown` 按记录回滚；`Restore` 用于 GUI
//!    崩溃后由下次启动回滚遗留状态。
//!
//! 帧格式：**一个 `SOCK_SEQPACKET` 报文 = 一个 UTF-8 JSON 消息**，
//! 单条上限 [`MAX_MESSAGE`] 字节。
//!
//! 选 `SOCK_SEQPACKET` 而不是 `SOCK_STREAM` 是刻意的：
//!
//! * 保留了消息边界，于是**不需要自己写长度前缀**，也就没有「粘包/半包」这一
//!   整类 bug；
//! * 同时仍然支持 `SCM_RIGHTS`，可以把 utun 的 fd 直接附在同一条消息上发出去。
//!
//! 之所以用 JSON 而不是 protobuf：helper 的接口面很小、每秒消息数极低，
//! 可读性/可调试性比编码效率重要得多。

pub mod transport;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// 线协议版本。任何破坏兼容性的改动都要 +1，helper 会拒绝不等版本。
pub const PROTOCOL_VERSION: u32 = 1;

/// helper 监听的 Unix domain socket 路径。
///
/// 放在 `/var/run` 是为了让权限位（root:admin 0660）天然受限；
/// `/var/run` 在 macOS 上是 `/private/var/run` 的符号链接，重启后清空 —— 这正是我们想要的。
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/com.xraytun.helper.sock";

/// launchd 服务标签，同时也是 plist 文件名（`<label>.plist`）。
pub const HELPER_LABEL: &str = "com.xraytun.helper";

/// helper 可执行文件在 app bundle 内以及安装后的路径约定。
pub const HELPER_BUNDLE_SUBPATH: &str = "Contents/MacOS/xraytun-helper";
pub const HELPER_INSTALLED_PATH: &str = "/Library/PrivilegedHelperTools/com.xraytun.helper";
pub const HELPER_PLIST_PATH: &str = "/Library/LaunchDaemons/com.xraytun.helper.plist";

/// 单条消息最大字节数。正常请求远小于此值，超出即视为异常连接并断开。
pub const MAX_MESSAGE: usize = 64 * 1024; // 64 KiB

// ---------------------------------------------------------------------------
// 顶层消息
// ---------------------------------------------------------------------------

/// GUI -> helper 的请求。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// 握手。必须在同连接上作为第一个请求发送。
    Hello {
        client_version: String,
        protocol: u32,
        /// 调用方声明的用途，仅用于审计日志。
        client_name: String,
    },
    /// 查询 helper 与 TUN 会话状态。
    Status,
    /// 建立 TUN 并安装路由/DNS。
    TunUp(Box<TunUpRequest>),
    /// 按 `session_id` 回滚一个 TUN 会话的全部副作用。
    TunDown { session_id: String },
    /// 索取 utun 的 fd（仅 `DatapathPlan::HandoffFd` 模式有效）。
    ///
    /// 响应是 [`Response::TunFd`]，并**在该条 SEQPACKET 消息上附带 `SCM_RIGHTS`**。
    TakeTunFd { session_id: String },
    /// 两阶段启动的第二步：现在接管默认路由。
    ///
    /// **只有在数据面确认就绪之后才应该调用**，否则会造成流量黑洞。
    CommitRoutes { session_id: String },
    /// 回滚磁盘上记录的最后一个（可能是崩溃遗留的）会话。
    Restore,
    /// 重新读取数据面计数器。
    Stats { session_id: String },
    /// 卸载：停止数据面、回滚会话、移除 launchd 注册与自身二进制。
    Uninstall,
    /// 让 helper 进程退出（launchd 会按需重新拉起）。
    Shutdown,
}

/// helper -> GUI 的响应。每个请求恰好对应一个响应（无服务端主动推送）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Response {
    Hello(HelloInfo),
    Ok { message: Option<String> },
    Status(Box<HelperStatus>),
    Stats(DatapathStats),
    /// 回应 `Request::TakeTunFd`，**该消息同时通过 `SCM_RIGHTS` 附带 utun fd**。
    ///
    /// 接收方必须先尝试 `recv_fd`；拿不到 fd 说明 helper 处于
    /// `SpawnDatapath` / `NoDatapath` 模式，此时应改用 [`HelperStatus`] 里的信息。
    TunFd(TunFdInfo),
    Error(HelperError),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunFdInfo {
    pub session_id: String,
    pub interface: String,
    pub mtu: u16,
    /// 数据面必须用这个值去读取 IP 包（macOS 上恒为 `AF_INET` 或 `AF_INET6` 的
    /// 大端编码，且每个包前有 4 字节地址族头）。
    pub header_len: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloInfo {
    pub helper_version: String,
    pub protocol: u32,
    /// helper 二进制自身的 SHA-256，便于 GUI 检测版本漂移。
    pub binary_sha256: String,
    pub tun_active: bool,
    /// 磁盘上是否存在未清理的会话（提示 GUI 调 `Restore`）。
    pub stale_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelperStatus {
    pub helper_version: String,
    pub pid: u32,
    pub started_at_unix: u64,
    /// 已建立的 TUN 会话（正常情况下 0 或 1 个）。
    pub sessions: Vec<SessionStatus>,
    /// 数据面可执行文件是否可用（tun2socks 类）。
    pub datapath_available: bool,
    pub datapath_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionStatus {
    pub session_id: String,
    pub interface: String,
    pub mtu: u16,
    pub addresses: Vec<Cidr>,
    pub installed_routes: Vec<InstalledRoute>,
    pub dns_modified: bool,
    pub datapath_pid: Option<u32>,
    pub started_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatapathStats {
    pub session_id: String,
    pub uptime_secs: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// 当前活跃的 TCP 会话数（数据面若能提供）。
    pub active_connections: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelperError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 请求本身不合法（字段越界、CIDR 非法等）。
    InvalidRequest,
    /// 协议版本不匹配。
    ProtocolMismatch,
    /// 调用方未通过代码签名 / UID 校验。
    Unauthorized,
    /// 尚未握手就发了其它请求。
    NotHandshaken,
    /// 找不到对应会话。
    NoSuchSession,
    /// TUN 设备创建失败。
    TunCreateFailed,
    /// 路由 / DNS 配置失败（此时 helper 已尽力回滚）。
    NetworkConfigFailed,
    /// 数据面进程启动失败。
    DatapathFailed,
    /// 内部错误。
    Internal,
}

impl HelperError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl fmt::Display for HelperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for HelperError {}

// ---------------------------------------------------------------------------
// TUN 会话描述
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TunUpRequest {
    /// 幂等键。GUI 对同一次“启动”重试时必须复用同一个 id。
    pub session_id: String,
    /// `None` 表示由内核挑选 utun 编号。
    pub interface_name: Option<String>,
    pub mtu: u16,
    /// 分配给 utun 的隧道内地址，例如 `198.18.0.1/15`。
    pub addresses: Vec<Cidr>,
    pub routes: RoutePlan,
    pub dns: DnsPlan,
    pub datapath: DatapathPlan,
    /// **两阶段启动**：为 `true` 时，本次只建卡、配地址、装 bypass 路由，
    /// 把「接管默认路由」的那几条留到 [`Request::CommitRoutes`]。
    ///
    /// 为什么需要：如果一次性全做完，从「路由已接管」到「数据面就绪」之间会有
    /// 几百毫秒的**黑洞窗口** —— 流量被送进一个还没人读的 utun，DNS 也已经切走，
    /// 用户会看到「一连上所有网页都打不开」。
    ///
    /// 推荐流程：
    /// ```text
    /// TunUp(defer = true) → TakeTunFd → 启动数据面 → 确认 SOCKS 端口就绪
    ///                     → CommitRoutes → 此时才真正开始接管流量
    /// ```
    #[serde(default)]
    pub defer_default_routes: bool,
}

/// 谁负责把 TUN 里的 IP 包搬到代理内核。
///
/// 注意「谁建 utun」和「谁跑数据面」是**两个独立的决定**，这里把它们合在一个
/// 枚举里是为了让协议保持扁平、避免非法组合。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DatapathPlan {
    /// helper 建 utun 并把 fd 通过 `SCM_RIGHTS` 交给调用方（**推荐**）。
    ///
    /// 调用方随后以**普通用户**身份运行数据面，例如把 fd 编号通过
    /// `XRAY_TUN_FD` 环境变量传给 Xray 的原生 `tun` 入站。
    /// 这样只有「建卡 + 改路由」这一小段逻辑以 root 运行。
    ///
    /// fd 需要用 [`Request::TakeTunFd`] 单独取（因为它必须走 `SCM_RIGHTS`）。
    HandoffFd,
    /// helper 建 utun、配置网络，并**以 root 拉起数据面进程**。
    ///
    /// 兼容性最好，代价是整个数据面都在 root 下运行。
    SpawnDatapath {
        /// 绝对路径。helper 会校验它位于允许的目录内。
        binary: String,
        /// 追加参数（helper 会再做一次白名单校验，且绝不经 shell 解析）。
        args: Vec<String>,
        /// `true`：把 helper 建好的 utun fd 通过 `XRAY_TUN_FD` 传给子进程，
        /// 子进程因此**跳过**自己的建卡/配地址/装路由逻辑。
        ///
        /// `false`：让子进程自建 utun（Xray 原生那条完全公开的路径）。
        /// 此时 helper 仍然负责快照与 DNS，但不应再重复创建 utun。
        use_helper_fd: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoutePlan {
    /// 覆盖默认路由的策略。
    pub default_route: DefaultRouteMode,
    /// 必须绕过隧道直连的主机（代理服务器自身的 IP），否则会形成路由环。
    pub bypass_hosts: Vec<IpAddr>,
    /// 必须绕过隧道的网段（RFC1918、链路本地、多播等）。
    pub bypass_networks: Vec<Cidr>,
    pub ipv6: Ipv6Mode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DefaultRouteMode {
    /// 不动默认路由，只加 `0.0.0.0/1` + `128.0.0.0/1`（推荐）。
    ///
    /// 这两条比 `default` 更具体，因此总是胜出；而原默认路由完好无损，
    /// 回滚只需删掉这两条，天然抗崩溃。
    SplitDefault,
    /// 删除原默认路由再指向 utun。更“干净”但回滚风险高，不建议。
    ReplaceDefault,
    /// 不改默认路由（仅测试用，流量不会进隧道）。
    None,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Ipv6Mode {
    /// 关闭 utun 上的 IPv6，让 v6 流量走物理网卡（可能泄漏，但不断网）。
    #[default]
    Passthrough,
    /// 同样用 `::/1` + `8000::/1` 接管 v6。
    Override,
    /// 显式禁用 v6（在 utun 上不配地址，并建议 GUI 关闭系统 v6）。
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DnsPlan {
    pub mode: DnsMode,
    /// 要写入系统的 DNS 服务器。TUN 模式下通常是“不可路由”的哨兵地址，
    /// 让所有 53 端口流量必然进入隧道。
    pub servers: Vec<IpAddr>,
    pub search_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum DnsMode {
    /// 自动作用于当前活跃的网络服务（由 helper 探测，通常是 Wi-Fi / Ethernet）。
    Automatic,
    /// 只改这些具名网络服务（`networksetup -listallnetworkservices` 的名字）。
    Explicit { services: Vec<String> },
    /// 不动系统 DNS（仅“系统代理”模式使用）。
    LeaveAlone,
}

/// helper 实际写入路由表的条目，用于精确回滚。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledRoute {
    pub destination: Cidr,
    /// `-interface utun4` 或 `-gateway 192.168.1.1`。
    pub via: RouteVia,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RouteVia {
    Interface { name: String },
    /// **带 `-ifscope` 的接口路由**：只对绑定了该接口的 socket 可见。
    ///
    /// macOS 允许多条同目的地、不同作用域的路由共存（`RTF_IFSCOPE`）。
    /// 这正是「TUN 接管默认路由」与「数据面自己走物理网卡」能同时成立的机制：
    ///
    /// ```text
    /// 普通 App（未绑定）        → 0.0.0.0/1  → utun          （进隧道）
    /// 数据面（IP_BOUND_IF=en0） → default    → 192.168.0.1   （走物理网卡）
    /// ```
    ///
    /// 没有它的话，`IP_BOUND_IF=en0` 的 socket 会因为「胜出路由在 utun 上」
    /// 直接拿到 `ENETUNREACH` —— 不是回退到默认路由，而是彻底失败。
    ///
    /// # `gateway` 为什么不能省
    ///
    /// 写成 `route add -net default -interface en0 -ifscope en0`（只有接口、没有网关）
    /// 会创建一条**纯接口路由**，内核视之为「目标在本地链路上」——
    /// 于是对 `1.1.1.1` 发 ARP，包根本出不去。实测症状是数据面所有直连
    /// 连接静默卡死。
    ///
    /// 必须把物理网关一起给出。
    ScopedInterface { name: String, gateway: IpAddr },
    Gateway { addr: IpAddr },
}

// ---------------------------------------------------------------------------
// 小工具类型
// ---------------------------------------------------------------------------

/// CIDR。`serde` 表示就是 `"10.0.0.0/8"` 这样的字符串。
///
/// **主机位原样保留，不做归一化。** 这一点很关键，而且很容易搞错：
///
/// * 用作**接口地址**时必须保留主机位 —— `198.18.0.1/15` 里的 `.1` 是接口
///   自己的地址；归一化成 `198.18.0.0/15` 就等于把「网络地址」配到了网卡上。
///   Xray 的 `tun.gateway = 169.254.10.1/30` 也是同样的语义。
/// * 用作**路由目标**时主机位没有意义，必须先归一化，否则 `route(8)` 的行为
///   依赖实现细节。这件事由 [`Cidr::network`] 显式完成，在安装路由前调用。
///
/// 把「归一化」从构造器里挪到显式方法，是因为这两种用途**同时存在**于本代码库，
/// 隐式归一化会在接口地址这条路径上静默地产生错误配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn new(addr: IpAddr, prefix: u8) -> Result<Self, CidrParseError> {
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(CidrParseError::PrefixTooLong { prefix, max });
        }
        Ok(Self { addr, prefix })
    }

    /// 清零主机位，得到网络地址。**安装路由前必须调用。**
    pub fn network(self) -> Self {
        let addr = match self.addr {
            IpAddr::V4(v4) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
                IpAddr::V4(Ipv4Addr::from(u32::from(v4) & mask))
            }
            IpAddr::V6(v6) => {
                let bits = u128::from(v6);
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix) };
                IpAddr::V6(Ipv6Addr::from(bits & mask))
            }
        };
        Self { addr, prefix: self.prefix }
    }

    /// 是否已经是对齐的网络地址（主机位全 0）。
    pub fn is_network(self) -> bool {
        self == self.network()
    }

    /// 该 CIDR 承载的本地主机地址。
    ///
    /// 起这个直白的名字，是为了让调用点「我要的是接口地址还是网段」
    /// 一眼可辨，而不是靠记忆。
    pub fn interface_addr(self) -> IpAddr {
        self.addr
    }

    /// 单主机网段（用于 `bypass_hosts`）。
    pub fn host(addr: IpAddr) -> Self {
        let prefix = if addr.is_ipv4() { 32 } else { 128 };
        Self { addr, prefix }
    }

    pub fn contains(&self, other: &Cidr) -> bool {
        if other.prefix < self.prefix {
            return false;
        }
        // 同族才可比。统一提升到 u128 计算掩码：v4 时高 96 位恒为 0，
        // 即使掩码在高位有 1 也不会影响比较结果。
        let (a, b, bits) = match (self.addr, other.addr) {
            (IpAddr::V4(a), IpAddr::V4(b)) => (u128::from(u32::from(a)), u128::from(u32::from(b)), 32u8),
            (IpAddr::V6(a), IpAddr::V6(b)) => (u128::from(a), u128::from(b), 128u8),
            _ => return false,
        };
        let mask = if self.prefix == 0 { 0 } else { u128::MAX << (bits - self.prefix) };
        (a & mask) == (b & mask)
    }

    /// 该网段是否是 IPv4 私有/特殊用途地址（用于默认的 bypass 列表校验）。
    pub fn is_v4_private(&self) -> bool {
        const PRIVATE: &[&str] = &[
            "0.0.0.0/8",
            "10.0.0.0/8",
            "100.64.0.0/10",
            "127.0.0.0/8",
            "169.254.0.0/16",
            "172.16.0.0/12",
            "192.168.0.0/16",
            "224.0.0.0/4",
            "240.0.0.0/4",
            "255.255.255.255/32",
        ];
        PRIVATE.iter().any(|p| p.parse::<Cidr>().map(|c| c == *self).unwrap_or(false))
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl std::str::FromStr for Cidr {
    type Err = CidrParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let (addr_part, prefix_part) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: IpAddr = addr_part.parse().map_err(|_| CidrParseError::BadAddr(s.to_string()))?;
        let prefix = match prefix_part {
            Some(p) => p.parse::<u8>().map_err(|_| CidrParseError::BadPrefix(p.to_string()))?,
            None => {
                if addr.is_ipv4() {
                    32
                } else {
                    128
                }
            }
        };
        Cidr::new(addr, prefix)
    }
}

impl TryFrom<String> for Cidr {
    type Error = CidrParseError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Cidr> for String {
    fn from(c: Cidr) -> String {
        c.to_string()
    }
}

impl Serialize for Cidr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Cidr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CidrParseError {
    #[error("不是合法的 IP 或 CIDR: {0}")]
    BadAddr(String),
    #[error("非法前缀长度: {0}")]
    BadPrefix(String),
    #[error("前缀长度 {prefix} 超出该地址族上限 {max}")]
    PrefixTooLong { prefix: u8, max: u8 },
}

// ---------------------------------------------------------------------------
// 默认值 / 常用集合
// ---------------------------------------------------------------------------

/// TUN 会话的默认隧道网段。
///
/// `198.18.0.0/15` 是 RFC 2544 保留的基准测试网段，公网不可路由，
/// 因此不会和用户真实网络冲突 —— 这也是 Clash / sing-box 的通行选择。
pub const DEFAULT_TUN_NETWORK_V4: &str = "198.18.0.1/15";
/// 哨兵 DNS：位于隧道网段内但不需要真的被解析，唯一目的是让查询进入 TUN。
pub const DEFAULT_SENTINEL_DNS_V4: &str = "198.18.0.2";
pub const DEFAULT_MTU: u16 = 1500;

/// 默认的“直连绕过”网段：这些地址永远不该走隧道。
pub fn default_bypass_networks() -> Vec<Cidr> {
    [
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "224.0.0.0/4",
        // 这里**没有** 255.255.255.255/32。
        //
        // 受限广播地址不是一个可路由的单播目的地，`route(8)` 会直接拒绝：
        //     route: bad address: 255.255.255.255/32
        // 而且它本来就不需要绕过隧道 —— 发往它的包只在本地链路投递，
        // 永远不会进入路由决策。
        "fe80::/10",
    ]
    .iter()
    .filter_map(|s| s.parse().ok())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_preserves_host_bits() {
        // 接口地址必须保留主机位：198.18.0.1/15 的 `.1` 是接口自己的地址。
        let c: Cidr = "198.18.0.1/15".parse().unwrap();
        assert_eq!(c.to_string(), "198.18.0.1/15");
        assert_eq!(c.interface_addr(), "198.18.0.1".parse::<IpAddr>().unwrap());
        assert!(!c.is_network());

        let c: Cidr = "192.168.1.1".parse().unwrap();
        assert_eq!(c.to_string(), "192.168.1.1/32");
        let c: Cidr = "::1".parse().unwrap();
        assert_eq!(c.to_string(), "::1/128");
    }

    #[test]
    fn network_clears_host_bits() {
        let c: Cidr = "198.18.0.1/15".parse().unwrap();
        assert_eq!(c.network().to_string(), "198.18.0.0/15");
        assert!(c.network().is_network());
        // 归一化是幂等的
        assert_eq!(c.network().network(), c.network());
        // /32 与 /128 本来就对齐
        assert!(Cidr::host("1.2.3.4".parse().unwrap()).is_network());
    }

    #[test]
    fn cidr_rejects_bad_prefix() {
        assert!("10.0.0.0/33".parse::<Cidr>().is_err());
        assert!("::/129".parse::<Cidr>().is_err());
        assert!("not-an-ip/8".parse::<Cidr>().is_err());
    }

    #[test]
    fn cidr_contains() {
        let outer: Cidr = "10.0.0.0/8".parse().unwrap();
        let inner: Cidr = "10.1.0.0/16".parse().unwrap();
        let other: Cidr = "11.0.0.0/8".parse().unwrap();
        assert!(outer.contains(&inner));
        assert!(!outer.contains(&other));
        assert!(!inner.contains(&outer));
    }

    /// 回归：`255.255.255.255/32` 不在 bypass 列表里。
    ///
    /// 它曾经在，结果 helper 装路由时直接被 `route(8)` 拒绝
    /// （`route: bad address`），**整个 TUN 建立失败**。
    #[test]
    fn default_bypass_networks_excludes_unroutable_broadcast() {
        for c in default_bypass_networks() {
            assert_ne!(
                c.addr.to_string(),
                "255.255.255.255",
                "受限广播地址不可路由，route(8) 会拒绝它"
            );
        }
    }

    #[test]
    fn request_roundtrips_through_json() {
        let req = Request::TunUp(Box::new(TunUpRequest {
            session_id: "s-1".into(),
            interface_name: None,
            mtu: DEFAULT_MTU,
            addresses: vec![DEFAULT_TUN_NETWORK_V4.parse().unwrap()],
            routes: RoutePlan {
                default_route: DefaultRouteMode::SplitDefault,
                bypass_hosts: vec!["203.0.113.7".parse().unwrap()],
                bypass_networks: default_bypass_networks(),
                ipv6: Ipv6Mode::Passthrough,
            },
            dns: DnsPlan {
                mode: DnsMode::Automatic,
                servers: vec![DEFAULT_SENTINEL_DNS_V4.parse().unwrap()],
                search_domains: vec![],
            },
            datapath: DatapathPlan::SpawnDatapath {
                binary: "/Library/PrivilegedHelperTools/tun2socks".into(),
                args: vec!["--proxy".into(), "socks5://127.0.0.1:10808".into()],
                use_helper_fd: true,
            },
            defer_default_routes: true,
        }));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"tun_up\""));
        assert!(json.contains("198.18.0.1/15"));
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn datapath_plan_handoff_is_a_bare_tag() {
        let json = serde_json::to_string(&DatapathPlan::HandoffFd).unwrap();
        assert_eq!(json, r#"{"mode":"handoff_fd"}"#);
        let back: DatapathPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, DatapathPlan::HandoffFd);
    }

    #[test]
    fn tun_fd_response_carries_metadata() {
        let r = Response::TunFd(TunFdInfo {
            session_id: "s-1".into(),
            interface: "utun4".into(),
            mtu: 1500,
            header_len: 4,
        });
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"status\":\"tun_fd\""));
        assert!(json.contains("\"header_len\":4"));
    }

    #[test]
    fn response_tag_is_stable() {
        let r = Response::Error(HelperError::new(ErrorCode::Unauthorized, "nope"));
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"status\":\"error\""));
        assert!(json.contains("\"code\":\"unauthorized\""));
    }
}
