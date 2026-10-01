//! xt-xrayconf —— Xray 配置生成：节点 + 设置 → 上游 JSON，纯函数
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! **纯函数**：不读文件、不碰进程、不查时钟。同样的输入一定得到同样的输出，
//! 所以「生成的配置是否合法」可以在单元测试里断言，也可以在真核心上跑
//! `xray run -test -c`（那是另一层验证，不是这一层的替代）。
//!
//! 这个 crate 只认识 xt-contract（不认识 `ParsedNode`）：配置生成只关心
//! 「一个 tag + 一段 outbound JSON」，节点是怎么解析来的与它无关。
//!
//! 两个模式各有一个入口：
//!
//! * [`generate`] —— proxy：socks 入站 + API 入站，出站是「选中的节点」与 `direct`；
//! * [`generate_tun`] —— TUN：xray 原生 `tun` 入站（utun 的 fd 经环境变量
//!   `XRAY_TUN_FD` 交给核心，**配置里不写 fd**）+ API 入站。
//!
//! **不做热更新**：切节点 = 重新生成整份配置 + 重启核心
//! （配置是纯函数的产物，重启是最不容易出错的落地方式；热更新会引入一份
//! 「配置里说的」与「核心正在跑的」不一致的中间状态）。
//!
//! # TUN 配置里 `autoSystemRoutingTable` 为什么留空
//!
//! TUN 网卡、地址与系统路由都由特权 helper 创建/安装，fd 也是 helper 交给核心的。
//! 如果配置里再让核心自己写一份 `autoSystemRoutingTable`，系统路由表上就会存在
//! **两套互相不知情的记录**：helper 的快照只记它装的那几条，核心崩溃时走它自己的
//! 清理逻辑删它那几条。只要有一方没跑到清理，就会留下**没人认领的路由** ——
//! 用户看到的现象是断网，且没有任何线索指向是谁留下的。
//!
//! 规则是「谁持有快照，谁负责路由」，那个角色是 helper。所以 TUN 配置里显式写一个
//! **空数组**（而不是省略字段）：把「本配置刻意不接管系统路由」变成配置里看得见、
//! 可以被断言的事实。防路由环不靠路由表，而靠 `autoOutboundsInterface` 与
//! `direct` 出站的 `sockopt.interface`（见 [`generate_tun`]）。

use std::net::{IpAddr, SocketAddr};

use serde_json::{json, Map, Value};
use xt_contract::error::{bad_request, ErrorBody, ErrorCode};
use xt_contract::model::{LogLevel, NodeId};

/// outbound 的 tag 前缀。`node-<id>` 里的 id 是稳定派生的 NodeId，
/// 所以同一个节点在任何一次生成里都得到同一个 tag —— 统计（StatsService）
/// 与路由规则因此可以长期对齐。
pub const NODE_TAG_PREFIX: &str = "node-";
/// socks 入站的 tag。backend-3 按它过滤统计，不能随手改。
pub const SOCKS_TAG: &str = "socks";
/// API 入站的 tag。
pub const API_TAG: &str = "api";
/// 直连出站的 tag。
pub const DIRECT_TAG: &str = "direct";
/// TUN 入站的 tag。backend-3 按它过滤统计，不能随手改。
pub const TUN_TAG: &str = "tun-in";

/// MTU 下限。低于这个值（例如 576）会把 TCP 握手都切碎，数据面表现为「能连上、
/// 但一传就断」，而且会被误判成节点问题。上限由 `u16` 本身保证（65535）。
const MTU_MIN: u16 = 1200;

/// 节点出站。`outbound` 是**内容**（protocol / settings / streamSettings），
/// tag 由本 crate 统一写入 —— 「一个节点一个 tag」只有一个地方说了算。
#[derive(Clone, Debug, PartialEq)]
pub struct OutboundSpec {
    pub node_id: NodeId,
    pub outbound: Value,
}

/// `generate` 的全部输入。`api_listen` 由 daemon 计算（socks 端口 + 1），
/// 不暴露成用户设置：用户改一个端口却忘了另一个，就会得到一个自己连不上的核心。
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigInputs {
    pub listen_socks: SocketAddr,
    pub api_listen: SocketAddr,
    pub selected: OutboundSpec,
    pub log_level: LogLevel,
}

/// [`generate_tun`] 的全部输入（与 [`ConfigInputs`] 并列，proxy 不受影响）。
///
/// 这里**没有** utun 的名字与 fd：名字让内核挑（避免与用户已有的 utun 冲突），
/// fd 由 helper 经环境变量 `XRAY_TUN_FD` 传给核心 —— fd 是进程的属性，不是配置的。
#[derive(Clone, Debug, PartialEq)]
pub struct TunConfigInputs {
    /// API 入站的监听地址（StatsService 用），只允许回环。
    pub api_listen: SocketAddr,
    pub selected: OutboundSpec,
    pub log_level: LogLevel,
    /// TUN 网段地址，如 `["198.18.0.1/15"]`。写进 Xray 的 `gateway` 字段。
    pub addresses: Vec<String>,
    /// utun 网关 = `addresses[0]` 的主机部分（如 `198.18.0.1`）。
    pub gateway: String,
    pub mtu: u16,
    /// 隧道 DNS 哨兵（如 `198.18.0.2`）。可空；为空时不写 `dns` 字段。
    pub dns_server_addr: String,
    /// 物理网卡名（如 `en0`）：出站 socket 绑到它，防路由环。
    pub physical_interface: String,
}

/// 生成主实例配置（proxy 模式）。
pub fn generate(input: &ConfigInputs) -> Result<String, ErrorBody> {
    let socks = validate_socks(input.listen_socks)?;
    let api = validate_api(input.listen_socks, input.api_listen)?;
    let node_tag = node_tag(&input.selected.node_id);

    let mut root = Map::new();
    root.insert("log".into(), log_section(input.log_level));
    root.insert("api".into(), api_section());
    root.insert("stats".into(), json!({}));
    root.insert("policy".into(), policy_section());
    root.insert(
        "inbounds".into(),
        json!([
            {
                "tag": SOCKS_TAG,
                "listen": socks.ip().to_string(),
                "port": socks.port(),
                "protocol": "socks",
                // udp: 保留 UDP，数据面（将来 tun2socks / QUIC）要用；
                // noauth 是因为它只监听回环。
                "settings": { "auth": "noauth", "udp": true, "userLevel": 0 }
            },
            {
                "tag": API_TAG,
                "listen": api.ip().to_string(),
                "port": api.port(),
                "protocol": "dokodemo-door",
                "settings": { "address": api.ip().to_string() }
            }
        ]),
    );
    root.insert(
        "outbounds".into(),
        json!([
            outbound_with_tag(&input.selected)?,
            { "tag": DIRECT_TAG, "protocol": "freedom", "settings": {} }
        ]),
    );
    root.insert(
        "routing".into(),
        json!({
            "domainStrategy": "IPIfNonMatch",
            "rules": [
                // API 流量自己不能被代理，否则查询统计会先经过隧道（还会把
                // 「查询本身」算进字节数）。
                { "type": "field", "inboundTag": [API_TAG], "outboundTag": API_TAG },
                { "type": "field", "inboundTag": [SOCKS_TAG], "outboundTag": node_tag }
            ]
        }),
    );
    serialize(root)
}

/// 生成探测用的临时实例配置：**每个节点一个独立的 socks 入站端口**，
/// 这样多个节点可以并行探测，而每条探测走哪个节点由入站 tag 决定，
/// 不需要在探测过程中改配置或重启（那会引入等待与竞态）。
///
/// 返回 `(配置, 端口列表)`，端口列表与 `outbounds` 一一对应。
pub fn generate_probe(outbounds: &[OutboundSpec], base_port: u16) -> Result<(String, Vec<u16>), ErrorBody> {
    if outbounds.is_empty() {
        return Err(bad_request("探测配置没有任何出站，生成它没有意义"));
    }
    if base_port == 0 {
        // 端口 0 表示「让内核随机挑」，那我们就无法把真实端口告诉调用方，
        // 返回一个猜的端口就是假数据。
        return Err(bad_request("探测基准端口不能是 0（0 由内核随机分配，调用方无法得知真实端口）"));
    }
    let count = u16::try_from(outbounds.len())
        .map_err(|_| bad_request(format!("节点数 {} 超出探测端口可用范围", outbounds.len())))?;
    // 先把整个端口区间验证完再生成：生成到一半才发现溢出，会留下一份
    // 只覆盖了部分节点的配置。
    base_port
        .checked_add(count - 1)
        .ok_or_else(|| bad_request(format!("探测端口区间超出 u16：base_port={base_port}，节点数={count}")))?;

    let mut inbounds = Vec::with_capacity(outbounds.len());
    let mut rules = Vec::with_capacity(outbounds.len());
    let mut ports = Vec::with_capacity(outbounds.len());
    let mut rendered = Vec::with_capacity(outbounds.len() + 1);

    for (index, spec) in outbounds.iter().enumerate() {
        let port = base_port + index as u16;
        let inbound_tag = format!("socks-{index}");
        ports.push(port);
        inbounds.push(json!({
            "tag": inbound_tag,
            "listen": "127.0.0.1",
            "port": port,
            "protocol": "socks",
            "settings": { "auth": "noauth", "udp": false, "userLevel": 0 }
        }));
        rules.push(json!({
            "type": "field",
            "inboundTag": [inbound_tag],
            "outboundTag": node_tag(&spec.node_id)
        }));
        rendered.push(outbound_with_tag(spec)?);
    }
    rendered.push(json!({ "tag": DIRECT_TAG, "protocol": "freedom", "settings": {} }));

    let mut root = Map::new();
    // 探测只需要真实 TTFB，日志噪音没有价值。
    root.insert("log".into(), json!({ "loglevel": "warning", "access": "", "error": "" }));
    root.insert("inbounds".into(), Value::Array(inbounds));
    root.insert("outbounds".into(), Value::Array(rendered));
    root.insert(
        "routing".into(),
        json!({ "domainStrategy": "IPIfNonMatch", "rules": Value::Array(rules) }),
    );
    Ok((serialize(root)?, ports))
}

/// 生成 TUN 模式的配置：xray 原生 `tun` 入站 + API 入站 + 选中节点 + `direct`。
///
/// # utun 的 fd 不在配置里
///
/// fd 由特权 helper 打开，经环境变量 `XRAY_TUN_FD` 传给核心（xray 在 macOS/Linux
/// 上支持这种「外部喂 fd」的用法）。fd 是进程的属性，写进配置文件只会得到一份
/// 换个进程就失效的配置。
///
/// # `autoSystemRoutingTable` 留空（见 crate 顶部注释）
///
/// 网卡与系统路由都由 helper 持有快照并负责回滚，所以这里显式写空数组，让核心
/// **跳过**它自己的地址/路由配置。写的是一个空数组而不是省略字段：省略是「没表态」，
/// 空数组是「明确表态不接管」，两者在排查时不一样。
///
/// # 防路由环
///
/// 路由由 helper 装进 utun 之后，核心自己发起的直连流量（含国内 DNS 的上游查询）
/// 会被默认路由送回隧道，形成环。两道防线：
///
/// 1. `settings.autoOutboundsInterface = physical_interface` —— 核心把出站绑到物理
///    网卡（等价于给所有出站批量设 `sockopt.interface`，并额外覆盖内建 DNS 那些
///    没有出站设置的请求）；
/// 2. `direct` 出站**逐一显式**写 `streamSettings.sockopt.interface` —— 上游实测
///    那道全局兜底在出站 socket 上并非总是生效，显式写的这条不依赖全局状态。
///
/// 字段名以 Xray 的 `infra/conf/tun.go` 为准（`gateway`/`dns` 是**数组**、
/// `autoSystemRoutingTable`/`autoOutboundsInterface` 是驼峰）。写成别的形状
/// （例如把 `gateway` 写成字符串）会让核心在配置解析阶段直接拒绝启动。
pub fn generate_tun(input: &TunConfigInputs) -> Result<String, ErrorBody> {
    validate_tun(input)?;

    let api = input.api_listen;
    let node_tag = node_tag(&input.selected.node_id);

    // tun 入站的 settings。键名/类型必须与 Xray 完全一致。
    let mut tun_settings = Map::new();
    tun_settings.insert("mtu".into(), json!(input.mtu));
    // userLevel 与 policy.levels 的键对应；0 是我们唯一的策略档。
    tun_settings.insert("userLevel".into(), json!(0));
    // `addresses` 对应 Xray 的 `gateway`（**数组**，每项一个 CIDR）：它是「分配给
    // TUN 网卡的地址前缀」，不是下一跳。macOS 上只取第一个 IPv4 前缀。
    tun_settings.insert(
        "gateway".into(),
        Value::Array(input.addresses.iter().cloned().map(Value::String).collect()),
    );
    if !input.dns_server_addr.trim().is_empty() {
        // `dns` 只在 Windows 上生效（macOS 忽略它）—— 但它是 Xray 认识的字段，
        // 写进去不会让配置非法；隧道 DNS 真正生效靠 helper 改系统 DNS。
        tun_settings.insert(
            "dns".into(),
            Value::Array(vec![Value::String(input.dns_server_addr.clone())]),
        );
    }
    // 显式空数组：helper 装路由，核心不要碰系统路由表。
    tun_settings.insert("autoSystemRoutingTable".into(), json!([]));
    tun_settings.insert(
        "autoOutboundsInterface".into(),
        Value::String(input.physical_interface.clone()),
    );

    // `direct` 出站显式绑物理网卡（防环的确定性那一层）。
    let mut direct_sockopt = Map::new();
    direct_sockopt.insert("interface".into(), Value::String(input.physical_interface.clone()));

    let mut root = Map::new();
    root.insert("log".into(), log_section(input.log_level));
    root.insert("api".into(), api_section());
    root.insert("stats".into(), json!({}));
    root.insert("policy".into(), policy_section());
    root.insert(
        "inbounds".into(),
        json!([
            {
                "tag": TUN_TAG,
                "protocol": "tun",
                "settings": Value::Object(tun_settings),
                // tcpFastOpen 关掉：TUN 里每个连接都是本机新建的，没有可复用的
                // 握手，开着只会让首包在残缺时被静默丢弃。
                "streamSettings": { "sockopt": { "tcpFastOpen": false } },
                // 嗅探出真实域名，路由才能按域名而不是按 IP 分流。
                "sniffing": { "enabled": true, "destOverride": ["http", "tls"] }
            },
            {
                "tag": API_TAG,
                "listen": api.ip().to_string(),
                "port": api.port(),
                "protocol": "dokodemo-door",
                "settings": { "address": api.ip().to_string() }
            }
        ]),
    );
    root.insert(
        "outbounds".into(),
        json!([
            outbound_with_tag(&input.selected)?,
            {
                "tag": DIRECT_TAG,
                "protocol": "freedom",
                "settings": {},
                "streamSettings": { "sockopt": Value::Object(direct_sockopt) }
            }
        ]),
    );
    root.insert(
        "routing".into(),
        json!({
            "domainStrategy": "IPIfNonMatch",
            "rules": [
                // 与 proxy 同一理由：API 流量不能被代理。
                { "type": "field", "inboundTag": [API_TAG], "outboundTag": API_TAG },
                // 隧道里的流量全部走选中的节点。`direct` 不需要规则 —— 它的出站
                // 已经绑了物理网卡，规则再写一遍只会多一个出错的地方。
                { "type": "field", "inboundTag": [TUN_TAG], "outboundTag": node_tag }
            ]
        }),
    );
    serialize(root)
}

/// TUN 输入的校验。全部通过才生成配置：非法输入在这里变成 `InvalidRequest`，
/// 而不是让核心在解析阶段报一句指不到输入的错误。
///
/// API 这里**没有**复用 [`validate_api`]：那条规则还要求「API 端口 = socks 端口 + 1」，
/// 而 TUN 模式没有 socks 入站，端口号由调用方决定。所以只保留两者共同的那条不变量：
/// **API 必须只监听回环**（配置里没有任何鉴权，监听非回环等于把统计接口开放出去）。
fn validate_tun(input: &TunConfigInputs) -> Result<(), ErrorBody> {
    if !input.api_listen.ip().is_loopback() {
        return Err(bad_request(format!("API 入站必须监听回环地址，收到 {}", input.api_listen))
            .with_detail(json!({ "api_listen": input.api_listen.to_string() })));
    }

    let Some(first_address) = input.addresses.first() else {
        return Err(bad_request("TUN 配置至少要有一个网段地址（如 198.18.0.1/15）"));
    };
    // 先把**每个**地址都验完再取用：只验第一个的话，第二个垃圾地址会一路走到
    // 核心才报错，现场就变成了「配置解析失败」，指不到是我们的输入错了。
    for address in &input.addresses {
        cidr_host(address)?;
    }

    let gateway: IpAddr = input.gateway.trim().parse().map_err(|_| {
        bad_request(format!("TUN 网关不是合法 IP：{}", input.gateway))
            .with_detail(json!({ "gateway": input.gateway.clone() }))
    })?;
    let expected = cidr_host(first_address)?;
    if gateway != expected {
        return Err(bad_request(format!(
            "TUN 网关必须等于第一个网段地址的主机部分（期望 {expected}，收到 {gateway}）"
        ))
        .with_detail(json!({ "gateway": input.gateway.clone(), "address": first_address })));
    }

    if input.mtu < MTU_MIN {
        return Err(bad_request(format!(
            "TUN MTU {} 太小（必须 >= {MTU_MIN}，上限 65535）",
            input.mtu
        ))
        .with_detail(json!({ "mtu": input.mtu })));
    }

    if input.physical_interface.trim().is_empty() {
        return Err(bad_request(
            "TUN 模式必须给出物理网卡名：没有它，直连流量会被路由送回隧道形成环",
        ));
    }

    let dns = input.dns_server_addr.trim();
    if !dns.is_empty() && dns.parse::<IpAddr>().is_err() {
        return Err(bad_request(format!("隧道 DNS 哨兵不是合法 IP：{dns}"))
            .with_detail(json!({ "dns_server_addr": input.dns_server_addr.clone() })));
    }

    Ok(())
}

/// 校验一个 CIDR，返回它的主机部分（如 `198.18.0.1/15` → `198.18.0.1`）。
///
/// 只检查「形状合法」：地址能被解析、前缀在 0..=32（v4）/ 0..=128（v6）之内。
/// 不检查地址属于哪个段 —— 198.18.0.0/15 是基准测试保留段，换段是用户的自由。
fn cidr_host(value: &str) -> Result<IpAddr, ErrorBody> {
    let invalid = || {
        bad_request(format!("TUN 网段地址不是合法 CIDR：{value}"))
            .with_detail(json!({ "address": value }))
    };
    let Some((host, prefix)) = value.split_once('/') else {
        return Err(invalid());
    };
    let Ok(ip) = host.parse::<IpAddr>() else {
        return Err(invalid());
    };
    let Ok(prefix) = prefix.parse::<u8>() else {
        return Err(invalid());
    };
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return Err(bad_request(format!("TUN 网段前缀越界：{value}（最大 /{max}）"))
            .with_detail(json!({ "address": value })));
    }
    Ok(ip)
}

/// `node-<id>`。id 本身是 base64url，不含 `/` 等会在别处被当作路径的字符。
pub fn node_tag(id: &NodeId) -> String {
    format!("{NODE_TAG_PREFIX}{id}")
}

fn outbound_with_tag(spec: &OutboundSpec) -> Result<Value, ErrorBody> {
    let mut object = match &spec.outbound {
        Value::Object(map) => map.clone(),
        _ => {
            return Err(ErrorBody::new(
                ErrorCode::Internal,
                format!("节点 {} 的 outbound 不是 JSON 对象", spec.node_id),
            ))
        }
    };
    if !object.get("protocol").is_some_and(Value::is_string) {
        return Err(ErrorBody::new(
            ErrorCode::Internal,
            format!("节点 {} 的 outbound 缺少 protocol 字段", spec.node_id),
        ));
    }
    // 覆盖而不是信任输入里的 tag：tag 的唯一来源是这里，避免出现
    // 「生成器以为叫 A，调用方以为叫 B」的错位。
    object.insert("tag".into(), Value::String(node_tag(&spec.node_id)));
    Ok(Value::Object(object))
}

fn log_section(level: LogLevel) -> Value {
    // access/error 为空字符串 = 输出到 stdout/stderr，由 daemon 逐行捕获成事件。
    json!({ "loglevel": xray_log_level(level), "access": "", "error": "" })
}

/// API 段。只开 StatsService：其他服务（Handler/Logger/Routing）本轮没有消费者，
/// 开了就是一条没用的攻击面。proxy 与 TUN 两个模式共用同一份 —— 统计口径必须是
/// 同一个，否则界面上切换模式会看到两套数字。
fn api_section() -> Value {
    json!({ "tag": API_TAG, "services": ["StatsService"] })
}

/// policy 段：统计开关。两个模式共用，理由同上。
fn policy_section() -> Value {
    json!({
        "levels": { "0": { "statsUserUplink": true, "statsUserDownlink": true } },
        // 四个开关全开：少了任何一个，StatsService 不会返回对应方向的计数器，
        // 界面就只能显示「未采样」。
        "system": {
            "statsInboundUplink": true,
            "statsInboundDownlink": true,
            "statsOutboundUplink": true,
            "statsOutboundDownlink": true
        }
    })
}

/// 契约里的 `LogLevel::Warn` 线上字符串是 `warn`，而 Xray 只认 `warning`。
/// 这个映射是**格式翻译**，不是回落：写 `warn` 会被核心判为非法配置。
fn xray_log_level(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Error => "error",
        LogLevel::Warn => "warning",
        LogLevel::Info => "info",
        LogLevel::Debug => "debug",
    }
}

fn validate_socks(addr: SocketAddr) -> Result<SocketAddr, ErrorBody> {
    if !addr.ip().is_loopback() {
        // 本轮的入口只服务本机；监听非回环地址等于把用户的出口开放给整个局域网，
        // 而配置里没有任何鉴权。
        return Err(bad_request(format!("socks 入站必须监听回环地址，收到 {addr}"))
            .with_detail(json!({ "listen_socks": addr.to_string() })));
    }
    Ok(addr)
}

fn validate_api(socks: SocketAddr, api: SocketAddr) -> Result<SocketAddr, ErrorBody> {
    if !api.ip().is_loopback() {
        return Err(bad_request(format!("API 入站必须监听回环地址，收到 {api}"))
            .with_detail(json!({ "api_listen": api.to_string() })));
    }
    let expected = socks
        .port()
        .checked_add(1)
        .ok_or_else(|| bad_request("socks 端口已经是 65535，无法再留出 API 端口"))?;
    if api.port() != expected {
        return Err(bad_request(format!(
            "API 端口必须是 socks 端口 + 1（期望 {expected}，收到 {}）",
            api.port()
        ))
        .with_detail(json!({
            "listen_socks": socks.to_string(),
            "api_listen": api.to_string(),
        })));
    }
    Ok(api)
}

/// 美化 JSON：落盘的配置是给人排查的，缩进值这个字节。
fn serialize(root: Map<String, Value>) -> Result<String, ErrorBody> {
    serde_json::to_string_pretty(&Value::Object(root)).map_err(|err| {
        ErrorBody::new(ErrorCode::Internal, format!("生成的配置无法序列化: {err}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn spec(id: &str, protocol: &str) -> OutboundSpec {
        OutboundSpec {
            node_id: NodeId::new(id),
            outbound: json!({
                "protocol": protocol,
                "settings": { "vnext": [{ "address": "a.example.com", "port": 443, "users": [{ "id": "u", "encryption": "none" }] }] },
                "streamSettings": { "network": "tcp", "security": "none" }
            }),
        }
    }

    fn inputs(selected: OutboundSpec) -> ConfigInputs {
        ConfigInputs {
            listen_socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1080),
            api_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1081),
            selected,
            log_level: LogLevel::Info,
        }
    }

    /// 核心断言：socks 路由、outbound tag、NodeId 三者一一对应。
    #[test]
    fn socks_route_points_at_the_only_node_outbound_with_matching_id() {
        let node = spec("bm9kZS0x", "vless");
        let config: Value = serde_json::from_str(&generate(&inputs(node.clone())).expect("生成")).expect("合法 JSON");

        let outbounds = config["outbounds"].as_array().expect("outbounds 数组");
        assert_eq!(outbounds.len(), 2);
        let node_tag_value = node_tag(&node.node_id);
        assert_eq!(outbounds[0]["tag"], json!(node_tag_value));
        assert_eq!(outbounds[0]["protocol"], json!("vless"));
        assert_eq!(outbounds[1]["tag"], json!(DIRECT_TAG));

        let rules = config["routing"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        let socks_rule = rules.iter().find(|r| r["inboundTag"] == json!([SOCKS_TAG])).expect("socks 规则");
        assert_eq!(socks_rule["outboundTag"], json!(node_tag_value));

        // 一一对应：配置里只有一个 node-<id> tag，且它正是被选中的那个。
        let node_tags: Vec<&str> = outbounds
            .iter()
            .filter_map(|o| o["tag"].as_str())
            .filter(|t| t.starts_with(NODE_TAG_PREFIX))
            .collect();
        assert_eq!(node_tags, vec![node_tag_value.as_str()]);
        assert!(config["inbounds"].as_array().expect("inbounds").iter().any(|i| i["tag"] == json!(SOCKS_TAG)));
    }

    #[test]
    fn stats_and_api_are_wired_for_real_counters() {
        let config: Value = serde_json::from_str(&generate(&inputs(spec("a", "vless"))).expect("生成")).expect("JSON");
        assert_eq!(config["stats"], json!({}));
        assert_eq!(config["api"]["services"], json!(["StatsService"]));
        let api_inbound = config["inbounds"]
            .as_array()
            .expect("inbounds")
            .iter()
            .find(|i| i["tag"] == json!(API_TAG))
            .expect("api 入站");
        assert_eq!(api_inbound["protocol"], json!("dokodemo-door"));
        assert_eq!(api_inbound["port"], json!(1081));
        for key in ["statsInboundUplink", "statsInboundDownlink", "statsOutboundUplink", "statsOutboundDownlink"] {
            assert_eq!(config["policy"]["system"][key], json!(true), "{key} 必须为 true");
        }
    }

    #[test]
    fn wrong_api_port_is_refused_instead_of_guessed() {
        let mut input = inputs(spec("a", "vless"));
        input.api_listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9999);
        let err = generate(&input).expect_err("API 端口错必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
        assert!(err.message.contains("socks 端口 + 1"), "{err:?}");
    }

    #[test]
    fn non_loopback_listen_is_refused() {
        let mut input = inputs(spec("a", "vless"));
        input.listen_socks = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1080);
        let err = generate(&input).expect_err("非回环必须拒绝");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
    }

    #[test]
    fn probe_gives_one_port_per_node_routed_to_its_own_tag() {
        let specs = vec![spec("a", "vless"), spec("b", "trojan")];
        let (text, ports) = generate_probe(&specs, 21000).expect("生成探测配置");
        assert_eq!(ports, vec![21000, 21001]);
        let config: Value = serde_json::from_str(&text).expect("JSON");

        let inbounds = config["inbounds"].as_array().expect("inbounds");
        assert_eq!(inbounds.len(), 2);
        assert_eq!(inbounds[0]["port"], json!(21000));
        assert_eq!(inbounds[0]["tag"], json!("socks-0"));
        assert_eq!(inbounds[1]["tag"], json!("socks-1"));

        let rules = config["routing"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0]["inboundTag"], json!(["socks-0"]));
        assert_eq!(rules[0]["outboundTag"], json!(node_tag(&specs[0].node_id)));
        assert_eq!(rules[1]["outboundTag"], json!(node_tag(&specs[1].node_id)));
    }

    #[test]
    fn probe_refuses_overflow_and_empty_input() {
        assert!(generate_probe(&[], 20000).is_err());
        let specs = vec![spec("a", "vless"), spec("b", "trojan")];
        let err = generate_probe(&specs, 65535).expect_err("端口溢出必须报错");
        assert_eq!(err.code, ErrorCode::InvalidRequest, "{err:?}");
    }

    #[test]
    fn log_level_is_translated_to_what_xray_accepts() {
        let mut input = inputs(spec("a", "vless"));
        input.log_level = LogLevel::Warn;
        let config: Value = serde_json::from_str(&generate(&input).expect("生成")).expect("JSON");
        assert_eq!(config["log"]["loglevel"], json!("warning"));
    }

    // ---- TUN ----

    fn tun_inputs(selected: OutboundSpec) -> TunConfigInputs {
        TunConfigInputs {
            api_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1081),
            selected,
            log_level: LogLevel::Info,
            addresses: vec!["198.18.0.1/15".to_string()],
            gateway: "198.18.0.1".to_string(),
            mtu: 1500,
            dns_server_addr: "198.18.0.2".to_string(),
            physical_interface: "en0".to_string(),
        }
    }

    fn tun_config(input: &TunConfigInputs) -> Value {
        serde_json::from_str(&generate_tun(input).expect("生成 TUN 配置")).expect("合法 JSON")
    }

    fn tun_inbound(config: &Value) -> &Value {
        config["inbounds"]
            .as_array()
            .expect("inbounds")
            .iter()
            .find(|i| i["tag"] == json!(TUN_TAG))
            .expect("tun 入站")
    }

    /// 字段名与类型必须与 Xray 的 `infra/conf/tun.go` 一致。
    ///
    /// 这条测试的价值在于**钉住形状**：`gateway`/`dns` 是数组、其余是驼峰，
    /// 写成 `gateway: "198.18.0.1"` 之类的形状核心会直接拒绝启动，而那种错误
    /// 在单元测试里不报、只在真核心上炸。
    #[test]
    fn tun_settings_match_the_xray_schema() {
        let config = tun_config(&tun_inputs(spec("bm9kZS0x", "vless")));
        let tun = tun_inbound(&config);

        assert_eq!(tun["protocol"], json!("tun"));
        assert_eq!(tun["settings"]["mtu"], json!(1500));
        assert_eq!(tun["settings"]["userLevel"], json!(0));
        assert_eq!(tun["settings"]["gateway"], json!(["198.18.0.1/15"]));
        assert_eq!(tun["settings"]["dns"], json!(["198.18.0.2"]));
        // 显式空数组（不是省略字段）：路由由 helper 装，核心不碰系统路由表。
        assert_eq!(tun["settings"]["autoSystemRoutingTable"], json!([]));
        assert_eq!(tun["settings"]["autoOutboundsInterface"], json!("en0"));
        assert_eq!(tun["streamSettings"]["sockopt"]["tcpFastOpen"], json!(false));
        assert_eq!(tun["sniffing"]["enabled"], json!(true));
        assert_eq!(tun["sniffing"]["destOverride"], json!(["http", "tls"]));

        // 名字让内核挑（避免与用户已有的 utun 冲突）；fd 走 XRAY_TUN_FD，不进配置。
        assert!(tun["settings"].get("name").is_none(), "不该写死 utun 名字");
        assert!(tun["settings"].get("fd").is_none(), "fd 只能经环境变量传入");
    }

    /// TUN 入站 → 节点，API 入站 → api tag；`direct` 绑物理网卡防环。
    #[test]
    fn tun_routes_to_the_selected_node_and_binds_direct_to_the_physical_interface() {
        let node = spec("bm9kZS0x", "vless");
        let config = tun_config(&tun_inputs(node.clone()));
        let node_tag_value = node_tag(&node.node_id);

        let outbounds = config["outbounds"].as_array().expect("outbounds");
        assert_eq!(outbounds.len(), 2);
        assert_eq!(outbounds[0]["tag"], json!(node_tag_value));
        assert_eq!(outbounds[0]["protocol"], json!("vless"));
        assert_eq!(outbounds[1]["tag"], json!(DIRECT_TAG));
        assert_eq!(outbounds[1]["protocol"], json!("freedom"));
        assert_eq!(
            outbounds[1]["streamSettings"]["sockopt"]["interface"],
            json!("en0"),
            "direct 必须显式绑物理网卡，否则直连流量会被路由送回隧道形成环"
        );

        let rules = config["routing"]["rules"].as_array().expect("rules");
        assert_eq!(rules.len(), 2);
        let tun_rule = rules.iter().find(|r| r["inboundTag"] == json!([TUN_TAG])).expect("tun 规则");
        assert_eq!(tun_rule["outboundTag"], json!(node_tag_value));
        let api_rule = rules.iter().find(|r| r["inboundTag"] == json!([API_TAG])).expect("api 规则");
        assert_eq!(api_rule["outboundTag"], json!(API_TAG));

        assert_eq!(config["api"]["services"], json!(["StatsService"]));
        assert_eq!(config["stats"], json!({}));
        let api_inbound = config["inbounds"]
            .as_array()
            .expect("inbounds")
            .iter()
            .find(|i| i["tag"] == json!(API_TAG))
            .expect("api 入站");
        assert_eq!(api_inbound["protocol"], json!("dokodemo-door"));
        assert_eq!(api_inbound["port"], json!(1081));
    }

    #[test]
    fn tun_omits_dns_when_the_sentinel_is_empty() {
        let mut input = tun_inputs(spec("a", "vless"));
        input.dns_server_addr = "   ".to_string();
        let config = tun_config(&input);
        assert!(tun_inbound(&config)["settings"].get("dns").is_none(), "空哨兵不该写 dns 字段");
    }

    /// 每个非法输入都必须变成 `InvalidRequest`，而不是一份「能生成但核心拒绝」的配置。
    #[test]
    fn tun_refuses_invalid_inputs() {
        let node = spec("a", "vless");

        let mut input = tun_inputs(node.clone());
        input.addresses.clear();
        assert_eq!(generate_tun(&input).expect_err("空地址必须拒绝").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.addresses = vec!["198.18.0.1/15".into(), "垃圾".into()];
        assert_eq!(generate_tun(&input).expect_err("任何一项非法都要拒绝").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.addresses = vec!["198.18.0.1/33".into()];
        assert_eq!(generate_tun(&input).expect_err("前缀越界要拒绝").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.gateway = "not-an-ip".into();
        assert_eq!(generate_tun(&input).expect_err("网关必须是 IP").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.gateway = "198.18.0.9".into();
        assert_eq!(
            generate_tun(&input).expect_err("网关必须与第一个地址一致").code,
            ErrorCode::InvalidRequest
        );

        let mut input = tun_inputs(node.clone());
        input.mtu = 1000;
        assert_eq!(generate_tun(&input).expect_err("MTU 太小要拒绝").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.physical_interface = "   ".into();
        assert_eq!(generate_tun(&input).expect_err("缺物理网卡要拒绝").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.dns_server_addr = "dns.local".into();
        assert_eq!(generate_tun(&input).expect_err("DNS 哨兵必须是 IP").code, ErrorCode::InvalidRequest);

        let mut input = tun_inputs(node.clone());
        input.api_listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 1081);
        assert_eq!(generate_tun(&input).expect_err("API 非回环要拒绝").code, ErrorCode::InvalidRequest);
    }
}
