//! 数据类型。冻结于 `docs/design/MACOS-APP-PLAN.md` §5。
//!
//! 本 crate 只被特权 helper 调用。对外形状都在这里，不向 helper 泄漏
//! daemon / xray / 订阅的任何概念。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};

use xt_contract::error::{bad_request, ErrorBody};

/// 一条 CIDR。`"198.18.0.1/15"` 会**保留主机位**（接口地址需要主机位），
/// 需要网络号时显式调 [`Cidr::network`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl Cidr {
    pub fn parse(text: &str) -> Result<Self, ErrorBody> {
        let (addr, prefix) = text
            .split_once('/')
            .ok_or_else(|| bad_request(format!("CIDR 缺少 '/' 前缀：{text}")))?;
        let prefix: u8 = prefix
            .parse()
            .map_err(|_| bad_request(format!("CIDR 前缀不是整数：{prefix}")))?;
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| bad_request(format!("CIDR 地址不是合法 IP：{addr}")))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(bad_request(format!("CIDR 前缀 {prefix} 超过上限 {max}：{text}")));
        }
        Ok(Self { addr, prefix })
    }

    pub fn is_ipv4(&self) -> bool {
        self.addr.is_ipv4()
    }

    /// 网络号（地址 & 掩码）。装路由前必须归一化，避免把主机位喂给 `route(8)`。
    pub fn network(&self) -> IpAddr {
        match self.addr {
            IpAddr::V4(a) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
                IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask))
            }
            IpAddr::V6(a) => {
                let mask = if self.prefix == 0 { 0 } else { u128::MAX << (128 - self.prefix) };
                IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask))
            }
        }
    }

    /// IPv4 前缀长度 → 点分十进制掩码（`ifconfig netmask` 用）。
    pub fn netmask_v4(&self) -> Result<Ipv4Addr, ErrorBody> {
        match self.addr {
            IpAddr::V4(_) => {
                let mask = if self.prefix == 0 { 0 } else { u32::MAX << (32 - self.prefix) };
                Ok(Ipv4Addr::from(mask))
            }
            IpAddr::V6(_) => Err(bad_request(format!("不是 IPv4 CIDR：{self}"))),
        }
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// 一条路由的 via 方式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteVia {
    /// 接口路由：`route -n add <dest> -interface <name>`。
    Interface { name: String },
    /// 网关路由：`route -n add <dest> <gateway>`。
    Gateway { addr: IpAddr },
}

/// 一条**已经装到系统上**的路由（快照按安装顺序记录，回滚倒序撤销）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledRoute {
    pub destination: Cidr,
    pub via: RouteVia,
    /// 装这条之前，该前缀上原本的路由（回滚时若存在则恢复）。
    #[serde(default)]
    pub replaced: Option<RouteVia>,
}

/// 建卡请求。冻结于 §5。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunRequest {
    /// 接口地址，`"198.18.0.1/15"` 形式。
    pub addresses: Vec<String>,
    pub mtu: u16,
    /// 内网直连 + 网关 host 路由（建卡阶段就装，走物理网关）。
    pub bypass_routes: Vec<String>,
    /// `0.0.0.0/1` + `128.0.0.0/1`（提交阶段才装，走 utun 接口）。
    pub default_routes: Vec<String>,
    pub dns_servers: Vec<String>,
}

/// 建卡结果。冻结于 §5。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunSession {
    pub id: String,
    pub interface: String,
}

/// 会话快照状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// 正在建立，中途崩溃需要回滚。
    BringingUp,
    /// 已建立并稳定运行。
    Up,
    /// 正在拆除。
    TearingDown,
}

/// DNS 备份：改 DNS 前先记下「用户原值」。快照里存一份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnsBackup {
    /// 网络服务名（`networksetup` 的 service，例如 `"Wi-Fi"`）。
    pub service: String,
    /// 改前的 DNS 服务器（不含隧道哨兵，见 `imp::dns`）。
    pub servers: Vec<String>,
}

/// 物理上行链路（默认路由指向的网卡 + 网关）。helper 装旁路路由需要它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhysicalUplink {
    pub interface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<IpAddr>,
}

/// 会话快照：把「为了让 TUN 工作而对系统做的所有改动」记在磁盘上。
///
/// 为什么必须落盘：helper 可能被 `kill -9`、系统可能断电。副作用只记内存的话，
/// 下次启动没人知道「上次加了哪些路由、把 DNS 改成了什么」，用户会永久断网。
/// 所以每成功一步就落一次盘，崩溃后能精确回滚。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub session_id: String,
    pub interface: String,
    pub state: SessionState,
    /// 已配到接口上的地址（回滚时随 fd 关闭 / 接口销毁自然消失，仅作记录）。
    #[serde(default)]
    pub addresses: Vec<Cidr>,
    /// 已装的 bypass + default 路由（按安装顺序，回滚倒序）。
    #[serde(default)]
    pub installed_routes: Vec<InstalledRoute>,
    /// 两阶段里「已算好但还没装」的 default 路由。落盘是为了在
    /// `tun_up` 与 `commit_routes` 之间崩溃时也能判为未完成。
    #[serde(default)]
    pub pending_routes: Vec<InstalledRoute>,
    /// 要设置的 DNS 服务器（从 `TunRequest` 带来，`commit_routes` 时才用）。
    #[serde(default)]
    pub dns_servers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical: Option<PhysicalUplink>,
    /// `commit_routes` 时才写；`None` = DNS 还没动。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsBackup>,
}

impl Snapshot {
    /// 「崩在半路」：没干净拆掉，或两阶段启动中途崩了。
    pub fn is_stale(&self) -> bool {
        self.state != SessionState::Up || !self.pending_routes.is_empty()
    }
}
