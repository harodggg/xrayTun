//! 地球仪数据：本机与出口节点的地理位置。
//!
//! # 一个必须说清的边界
//!
//! **位置来自第三方 IP 库**（**8 个**坐标源：`ipwho.is` / `ip-api.com` / `ipinfo.io` /
//! `ifconfig.co` / `ipwhois.app` / `json.geoiplookup.io` / `freeipapi.com` /
//! `api.ipquery.io`），不是本地算出来的 —— 项目自带的 `geoip.dat` 只有国别与网段，
//! 没有经纬度（实测确认）。把节点 IP 发给第三方是个真实的代价，所以：结果**按公网 IP
//! 持久缓存**、界面上标注来源、查询失败时如实报错而不是画一个坐标 (0,0) 的假点。
//!
//! 八个源**并发**（`tokio::join!`），单次上限仍是 `curl_get` 的 `--max-time 8`，
//! 不叠加；任何一个挂掉都不影响结果（`sources` 里如实写「无结果」），全挂才返回 `None`。
//!
//! **查询必须绕过隧道**：隧道开着时直接发请求会从节点出去，查到的会是
//! 「节点自己的位置」—— 错得很像对的。详见 `xt_core::geo_lookup`。
//!
//! # 0.9.1：先探 IP，再决定查不查坐标
//!
//! 旧实现**每次进页面都真查网络**（用户抱怨的「重新加载」）。现在：
//!
//! 1. 先用两个**只取 IP** 的轻量源探当前公网 IP（`api.ipify.org`、
//!    `1.1.1.1/cdn-cgi/trace`）；
//! 2. 这个 IP 在 `<数据目录>/location-cache.json` 里已有记录、且这次不是强制刷新
//!    ⇒ **直接用缓存返回，一个坐标源都不请求**（`from_cache = true`）；
//! 3. IP 变了 / 没查过 / 强制刷新 ⇒ **四个**坐标源并发互校，写回缓存。
//!
//! 缓存是**加速层而非事实来源**：坏文件、缺文件、版本不符一律当空缓存（见
//! `xt_core::store::Store::load_location_cache`），最多导致重查一次。
//!
//! # ⚠️ 不要再加回 `ipapi.co`（task-11 实测）
//!
//! `https://ipapi.co/json/` 在真实网络下返回的是 **Cloudflare 挑战页**，不是 JSON：
//!
//! ```text
//! $ curl -sS -A 'XrayTun/location' https://ipapi.co/json/
//! <!DOCTYPE html><html lang="en-US"><head><title>Just a moment...</title>...
//! ```
//!
//! 解析器会安全地返回 `None`（不会造假），但这一源**等于没有** —— 白白多一个
//! 请求、多一次「把 IP 发给第三方」。换成了 `ipinfo.io/json` 与 `ifconfig.co/json`
//! （两者都实测返回 JSON，见 task-11 证据）。加新源前请先 `curl` 一次确认是 JSON。

use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::sync::Mutex;

use xt_core::geo_lookup::{parse_api, parse_who, GeoLocation, CONSISTENT_TOLERANCE_DEG};
use xt_core::store::{
    location_key, CachedLocation, LocationCache, LocationFailure, LocationKeyKind, LocationProbe,
    LocationStats, LOCATION_CACHE_MAX_ENTRIES, LOCATION_FAIL_COOLDOWN_S, LOCATION_PROBE_TTL_S,
};
use xt_core::util::now_unix;
use xt_core::xray::stats::{monotonic_traffic_by_tag, MonotonicCounters, StatEntry};

use super::*;

/// 地球仪上的一条航线。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeRoute {
    /// 起点（本机公网出口）。
    pub from: GeoLocation,
    /// 终点（出口节点）。
    pub to: GeoLocation,
    /// 这条航线当前承载的实测字节（上行+下行），用于决定飞机密度。
    ///
    /// **跨核心重启保持单调**；`traffic_ok == false` 时固定为 0，
    /// **不是**真实读数（此前正是这里把「没查到」显示成了 0）。
    pub bytes: u64,
    /// 这次到底查没查到流量。
    pub traffic_ok: bool,
    /// 累计值跨重启续接时补偿掉的归零次数（`>0` = 核心重启过）。
    pub counter_resets: u32,
    /// 出口节点的名字。
    pub node_name: String,
    /// 这条航线的流量**归属**（task-179 / A20）。
    pub traffic: TrafficProvenance,
}

/// 出口累计流量的**归属**（task-179 / A20）。
///
/// 旧实现取「所有出站里 up+down 最大的那个」当出口，注释自己写着
/// 「最大的那个就是节点出站」—— **那是假设，不是验证**：真凶可能是 `direct`，
/// 而用户会得出「我的节点扛了 8 GiB」这种数据结论。
///
/// 现在只认**具体 tag**：拿不到就如实 `unattributed`（`verified=false` + `reason` 必填）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrafficProvenance {
    /// 这个数值**真正来自哪个** outbound tag（未归属 = `None`）。
    pub tag: Option<String>,
    /// 该 tag 是不是**节点出站**（`node-*`）——不是的话界面不许说「我的节点」。
    pub is_node_outbound: bool,
    /// 归属是否**已验证**（拿到了具体 tag 且统计可用）。
    pub verified: bool,
    /// `verified = false` 时必填且具体。
    pub reason: Option<String>,
}

impl TrafficProvenance {
    /// 有具体 tag ⇒ 已验证。
    fn verified_for(tag: &str) -> Self {
        Self {
            tag: Some(tag.to_string()),
            is_node_outbound: tag.starts_with("node-"),
            verified: true,
            reason: None,
        }
    }

    /// 归不到任何 tag ⇒ 如实说「未归属」，并给出**具体**原因。
    fn unattributed(reason: impl Into<String>) -> Self {
        Self {
            tag: None,
            is_node_outbound: false,
            verified: false,
            reason: Some(reason.into()),
        }
    }
}

/// 「本机 · <IP>」这条陈述的**可验证来源**（task-179 / A21）。
///
/// 旧实现只在**读到**物理网卡时才加 `curl --interface`；读不到就走系统默认路由
/// —— **隧道开着时那就是从节点出去**，服务看到的是节点出口，而界面仍写「本机」。
/// 那是「陈述比事实强」，方向上还是隐私判断。
///
/// 现在把「来不来自本机」做成字段：只有**绑了物理网卡且拿到了位置**才 `trusted = true`；
/// 否则 `trusted = false` **且 `reason` 必填**（界面据此降级文案）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SelfCheck {
    /// 服务看到的那一个出口 IP（没问到 = `None`）。
    pub ip: Option<String>,
    /// **实际**绑定的物理网卡 —— 只有可信的那次查询才有值。
    pub bound_interface: Option<String>,
    /// 只有「绑了物理网卡 + 拿到了位置」才为 true。
    pub trusted: bool,
    /// `trusted = false` 时必填且具体。
    pub reason: Option<String>,
}

impl SelfCheck {
    /// 判据（**纯函数**：`字段 = X ⇒ 呈现 = Y` 的后端侧断言就钉在这里）。
    fn judge(iface: Option<&str>, origin: Option<&GeoLocation>) -> Self {
        match (iface, origin) {
            (Some(iface), Some(loc)) => Self {
                ip: Some(loc.ip.clone()),
                bound_interface: Some(iface.to_string()),
                trusted: true,
                reason: None,
            },
            (Some(iface), None) => Self {
                ip: None,
                bound_interface: None,
                trusted: false,
                reason: Some(format!(
                    "绑定 {iface} 的查询没有成功（两个数据源都没返回或绑卡失败）⇒ 本机位置未验证"
                )),
            },
            (None, Some(loc)) => Self {
                ip: Some(loc.ip.clone()),
                bound_interface: None,
                trusted: false,
                reason: Some(
                    "读不到物理默认路由 ⇒ 查询走的是系统默认路由；隧道开着时那就是节点出口，\
                     查到的不是本机"
                        .to_string(),
                ),
            },
            (None, None) => Self {
                ip: None,
                bound_interface: None,
                trusted: false,
                reason: Some(
                    "读不到物理默认路由，且两个数据源都没返回 ⇒ 本机位置未知".to_string(),
                ),
            },
        }
    }
}

/// 一次 `globe_data` 调用**用了缓存还是真查了**（0.9.1，接口冻结）。
///
/// 字段各自回答一个用户会问的问题，互不替代：
/// * 「这次是重新加载吗」→ `from_cache`；
/// * 「这份数据多久了」→ `fetched_unix` / `age_s`；
/// * 「为什么又查了一次」→ `ip_changed`；
/// * 「这份数据可信到什么程度」→ `stale` / `probe_cached` / `key_kind`。
///
/// `from_cache = true` 的**定义**是「本次一个坐标源都没请求」—— 不是「本机位置
/// 来自缓存」。出口节点位置若被重查，这个字段也必须是 `false`（否则界面会说
/// 「没有重新加载」，而事实上刚发过查询）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobeCacheInfo {
    /// true = 直接用了缓存，**没有**重新查询坐标。
    pub from_cache: bool,
    /// 缓存条目的抓取时间（Unix 秒）；本次刚查出来时为 None。
    pub fetched_unix: Option<u64>,
    /// 本次探测到的公网 IP 与缓存不同 ⇒ 触发了重新查询（首查也算 true）。
    ///
    /// 只在**探测成功**时才有意义：探测失败时这里一律 `false`，由 [`Self::stale`]
    /// 表达「无法确认」（把「没测到」说成「变了」是在断言没验证过的事）。
    pub ip_changed: bool,
    /// **无法确认**公网 IP 是否变化（探测失败 / 失败冷却），返回的是上一次的可用结果。
    ///
    /// 界面据此写「暂时无法确认公网 IP 是否变化」，而不是假装这是刚验证的读数。
    pub stale: bool,
    /// 本次**没有真探测**（短 TTL 记忆命中 ⇒ 连两个 IP-only 源都没发）。
    pub probe_cached: bool,
    /// 缓存键的粒度：`"ip"`（IPv4 / 归一化后的 mapped v6）或 `"ipv6-prefix"`（/64）。
    ///
    /// IPv6 时界面必须说明「位置粒度是 /64 前缀」—— 那个位置是给一个网段定的，
    /// 不是某一台设备。第三个取值 `"node-last"` 本轮**没有实现**（见 P2 的理由）。
    pub key_kind: String,
    /// 命中条目的年龄（秒）：**后端算好**，UI 不再自己减时钟（两台机器的时钟可能不同）。
    /// 本次是新查询时为 `None`。
    pub age_s: Option<u64>,
}

/// 地球仪数据。
#[derive(Debug, Clone, Serialize)]
pub struct GlobeData {
    pub route: Option<GlobeRoute>,
    /// 本机位置（即使没有出口也会尽量给出）。
    pub origin: Option<GeoLocation>,
    /// 拿不到位置时的原因，界面如实展示。
    pub error: Option<String>,
    /// 「本机 · IP」这条陈述的**可验证来源**（task-179 / A21）。
    pub self_check: SelfCheck,
    /// 这次到底用了缓存还是真查了（0.9.1）。
    pub cache: GlobeCacheInfo,
}

/// 取物理网卡名 —— 绑它才能绕过隧道，否则查询会从节点出去、查到节点的位置。
///
/// 隧道开着时默认路由就是 utun，所以这里必须读**物理**默认路由；
/// `default_route` 返回的是系统当前的默认路由，正是我们要绑的那个接口。
fn physical_interface() -> Option<String> {
    xt_tun::macos::route::default_route()
        .ok()
        .map(|d| d.interface)
}

/// 节点地址是否是私网/保留地址（这类地址没有公网位置，不该去查）。
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback(),
    }
}

// ---------------------------------------------------------------------------
// 单飞闸门 + 可注入的网络动作（0.9.1-C）
// ---------------------------------------------------------------------------

/// 进程内**单飞**闸门：同一时刻只有一个「本机位置查询」在飞。
///
/// 为什么需要：App 启动预热与用户点开「位置」页可能**同时**发生；没有闸门时
/// 两边会各跑一次四源查询（各把同一个 IP 发给四个第三方）。有了闸门，第二个
/// 调用会等第一个结束，然后**重新读缓存** ⇒ 短 TTL 记忆已经写好 ⇒ 一个请求都不发。
///
/// 用 `tokio::sync::Mutex` 而不是 `std`：它要跨 `.await` 持有。
async fn single_flight<F, Fut, T>(f: F) -> T
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    let gate = GATE.get_or_init(|| Mutex::new(()));
    let _guard = gate.lock().await;
    f().await
}

/// 盒装 future（带 `Send`）：让 [`LocationNet`] 作为 `&N` 注入，并让 Tauri 命令的
/// future 保持 `Send`。
type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 「位置」查询要用的**全部网络动作**。
///
/// 抽成 trait 只为一件事：**让整条决策序列可测** —— TTL 记忆、单飞、serve-stale、
/// 失败冷却都是**序列**行为，单测某个纯函数证明不了「打开 5 次只查 1 次」。
/// 与 `supervisor::TunUpOps` 同一手法（那里抽 trait 也只为了可测）。
/// 生产实现是 [`CurlNet`]，测试用计数替身。
trait LocationNet {
    fn probe(&self) -> BoxFut<'_, Option<String>>;
    fn lookup_self(&self, probed: Option<String>) -> BoxFut<'_, Option<GeoLocation>>;
    fn lookup_ip(&self, ip: String) -> BoxFut<'_, Option<GeoLocation>>;
}

/// 真实网络：系统 `curl`（`--max-time`）+ **物理网卡已知时一律绑卡**（A21 的根因）。
///
/// 绑卡这一条**一个字都不能改**：隧道开着时不绑卡，查到的会是节点自己的位置。
struct CurlNet {
    interface: Option<String>,
}

impl LocationNet for CurlNet {
    fn probe(&self) -> BoxFut<'_, Option<String>> {
        let iface = self.interface.clone();
        Box::pin(async move { query_public_ip(iface.as_deref()).await })
    }

    fn lookup_self(&self, probed: Option<String>) -> BoxFut<'_, Option<GeoLocation>> {
        let iface = self.interface.clone();
        Box::pin(async move { query_self(iface.as_deref(), probed.as_deref()).await })
    }

    fn lookup_ip(&self, ip: String) -> BoxFut<'_, Option<GeoLocation>> {
        let iface = self.interface.clone();
        Box::pin(async move { query_ip(&ip, iface.as_deref()).await })
    }
}

// ---------------------------------------------------------------------------
// 纯决策：key 规范化 / 探测 TTL / 失败冷却
// ---------------------------------------------------------------------------

/// 把 IP 规范成缓存键（IPv4 原样、IPv6 /64、mapped v6 → v4）。
/// 解析不出来时退回原始字符串：至少下回还是同一个键。
fn key_or_raw(ip: &str) -> (String, LocationKeyKind) {
    location_key(ip).unwrap_or_else(|| (ip.to_string(), LocationKeyKind::Ip))
}

/// `last_probe` 是否仍在**短 TTL** 内（L2）。
fn probe_memory_fresh(probe: Option<&LocationProbe>, now: u64) -> Option<&LocationProbe> {
    let p = probe?;
    (now.saturating_sub(p.checked_unix) <= LOCATION_PROBE_TTL_S).then_some(p)
}

/// `last_failure` 是否仍在**冷却**内（L4）。
fn in_failure_cooldown(failure: Option<&LocationFailure>, now: u64) -> Option<&LocationFailure> {
    let f = failure?;
    (now.saturating_sub(f.failed_unix) <= LOCATION_FAIL_COOLDOWN_S).then_some(f)
}

/// 从「本机位置来自缓存」的路径组装结果（探测记忆 / serve-stale / 失败冷却）。
///
/// `fetched_unix` / `age_s` 描述**本机位置那条缓存**：只要它是缓存来的就给时间；
/// `from_cache` 另外按「本次有没有发出坐标查询」算（出口节点位置可能被重查）。
fn served_from_cache(
    key: &str,
    kind: LocationKeyKind,
    entry: &CachedLocation,
    now: u64,
    stale: bool,
    probe_cached: bool,
    lookups_sent: u32,
) -> (GlobeCacheInfo, GeoLocation, SelfCheck) {
    let origin = cached_to_location(key, entry);
    let self_check = self_check_from_cache(entry, &origin);
    let info = GlobeCacheInfo {
        from_cache: lookups_sent == 0,
        fetched_unix: Some(entry.fetched_unix),
        ip_changed: false,
        stale,
        probe_cached,
        key_kind: kind.as_str().to_string(),
        age_s: Some(now.saturating_sub(entry.fetched_unix)),
    };
    (info, origin, self_check)
}

/// 出口节点位置：命中缓存就用缓存；`allow_network` 时才真查一次。
///
/// 出口查询**不参与**失败冷却：冷却记的是「本机位置那条四源全挂」，而节点位置
/// 跟着选中的节点走 —— 换节点就该重查，不该被上一台的失败挡住。
async fn resolve_exit<N: LocationNet + Sync>(
    net: &N,
    cache: &mut LocationCache,
    force: bool,
    exit_ip: Option<&str>,
    allow_network: bool,
    now: u64,
    lookups: &AtomicU32,
) -> Option<GeoLocation> {
    let ip = exit_ip?;
    let (key, _) = key_or_raw(ip);
    if !force {
        if let Some(entry) = cache.get(&key) {
            return Some(cached_to_location(&key, entry));
        }
    }
    if !allow_network {
        return None;
    }
    lookups.fetch_add(1, Ordering::Relaxed);
    let loc = net.lookup_ip(ip.to_string()).await?;
    cache.put(&key, location_to_cached(&loc, now, None));
    cache.evict_oldest_beyond(LOCATION_CACHE_MAX_ENTRIES);
    Some(loc)
}

/// 一次调用的结果（**决策层**：不含 route/traffic 组装与落盘）。
#[derive(Debug)]
struct CallOutcome {
    origin: Option<GeoLocation>,
    exit: Option<GeoLocation>,
    self_check: SelfCheck,
    cache: GlobeCacheInfo,
    error: Option<String>,
    /// 本次真的发了几次 IP 探测 / 几次坐标查询（给单测与统计用）。
    probe_sent: u32,
    lookups_sent: u32,
}

/// 一次调用的**完整决策序列**（可注入网络 ⇒ 可测）。
///
/// 顺序（每一步都有独立测试）：
/// A. 失败冷却（L4）→ 不再打网络，有上次结果就 stale 返回；
/// B. 探测短 TTL 记忆（L2）→ 连探测都不发；
/// C. 真探测 → 命中缓存直接返回；探测失败则 serve-stale（L3）；
/// D. 真查四源（出口查询与本机查询**并发**）。
async fn run_call<N: LocationNet + Sync>(
    net: &N,
    cache: &mut LocationCache,
    now: u64,
    force: bool,
    exit_ip: Option<&str>,
    iface: Option<&str>,
) -> CallOutcome {
    cache.stats.calls += 1;
    cache.stats.last_call_unix = now;
    let lookups = AtomicU32::new(0);
    let mut probe_sent = 0u32;

    // ---- A) 失败冷却 ----
    // `force` 是用户明确点的「重新定位」⇒ 绕过冷却：那是他的意图，不该被 60 秒挡住。
    if !force {
        if let Some(failure) = in_failure_cooldown(cache.last_failure.as_ref(), now).cloned() {
            cache.stats.probe_cached += 1;
            let exit = resolve_exit(net, cache, force, exit_ip, true, now, &lookups).await;
            let key_hit = if failure.ip.is_empty() {
                None
            } else {
                let (key, kind) = key_or_raw(&failure.ip);
                cache.get(&key).map(|e| (key, kind, e.clone()))
            };
            let lookups_sent = lookups.load(Ordering::Relaxed);
            return match key_hit {
                Some((key, kind, entry)) => {
                    cache.stats.hits += 1;
                    cache.stats.stale_hits += 1;
                    let (cache_info, origin, self_check) =
                        served_from_cache(&key, kind, &entry, now, true, true, lookups_sent);
                    CallOutcome {
                        origin: Some(origin),
                        exit,
                        self_check,
                        cache: cache_info,
                        error: None,
                        probe_sent,
                        lookups_sent,
                    }
                }
                None => CallOutcome {
                    origin: None,
                    exit,
                    self_check: SelfCheck::judge(iface, None),
                    cache: GlobeCacheInfo {
                        from_cache: lookups_sent == 0,
                        fetched_unix: None,
                        ip_changed: false,
                        stale: true,
                        probe_cached: true,
                        key_kind: "ip".to_string(),
                        age_s: None,
                    },
                    error: Some(format!(
                        "位置查询刚刚失败过（{reason}），{LOCATION_FAIL_COOLDOWN_S} 秒内不再重试",
                        reason = failure.reason
                    )),
                    probe_sent,
                    lookups_sent,
                },
            };
        }
    }

    // ---- B) 探测短 TTL 记忆 ----
    if !force {
        if let Some(probe) = probe_memory_fresh(cache.last_probe.as_ref(), now) {
            let (key, kind) = key_or_raw(&probe.ip);
            if let Some(entry) = cache.get(&key).cloned() {
                cache.stats.probe_cached += 1;
                cache.stats.hits += 1;
                let exit = resolve_exit(net, cache, force, exit_ip, true, now, &lookups).await;
                let lookups_sent = lookups.load(Ordering::Relaxed);
                let (cache_info, origin, self_check) =
                    served_from_cache(&key, kind, &entry, now, false, true, lookups_sent);
                return CallOutcome {
                    origin: Some(origin),
                    exit,
                    self_check,
                    cache: cache_info,
                    error: None,
                    probe_sent,
                    lookups_sent,
                };
            }
            // 记忆里的 IP 没有条目（被淘汰了）⇒ 继续走真探测，不装懂。
        }
    }

    // ---- C) 真探测 ----
    let probed = net.probe().await;
    probe_sent += 1;
    cache.stats.probe_calls += 1;
    if let Some(ip) = probed.as_deref() {
        cache.last_probe = Some(LocationProbe { ip: ip.to_string(), checked_unix: now });
    }

    // C1) 探测成功且命中 ⇒ 直接用缓存（一个坐标查询都不发）
    if let Some(ip) = probed.as_deref() {
        let (key, kind) = key_or_raw(ip);
        if !force {
            if let Some(entry) = cache.get(&key).cloned() {
                cache.stats.hits += 1;
                let exit = resolve_exit(net, cache, force, exit_ip, true, now, &lookups).await;
                let lookups_sent = lookups.load(Ordering::Relaxed);
                let (cache_info, origin, self_check) =
                    served_from_cache(&key, kind, &entry, now, false, false, lookups_sent);
                return CallOutcome {
                    origin: Some(origin),
                    exit,
                    self_check,
                    cache: cache_info,
                    error: None,
                    probe_sent,
                    lookups_sent,
                };
            }
        }
    } else if !force {
        // C2) serve-stale（L3）：探测失败，但上次探测到的 IP 有条目 ⇒ 返回它并标 stale，
        //     不发坐标查询 —— 别再让离线用户干等四个源。
        if let Some(last) = cache.last_probe.clone() {
            if !last.ip.is_empty() {
                let (key, kind) = key_or_raw(&last.ip);
                if let Some(entry) = cache.get(&key).cloned() {
                    cache.stats.stale_hits += 1;
                    cache.stats.hits += 1;
                    let exit = resolve_exit(net, cache, force, exit_ip, true, now, &lookups).await;
                    let lookups_sent = lookups.load(Ordering::Relaxed);
                    let (cache_info, origin, self_check) =
                        served_from_cache(&key, kind, &entry, now, true, false, lookups_sent);
                    return CallOutcome {
                        origin: Some(origin),
                        exit,
                        self_check,
                        cache: cache_info,
                        error: None,
                        probe_sent,
                        lookups_sent,
                    };
                }
            }
        }
    }

    // ---- D) 真查：出口查询与本机查询**并发**（与改前一样一次问完）----
    let exit_fut = resolve_exit(net, cache, force, exit_ip, true, now, &lookups);
    let self_fut = async {
        lookups.fetch_add(1, Ordering::Relaxed);
        net.lookup_self(probed.clone()).await
    };
    let (exit, fresh_origin) = tokio::join!(exit_fut, self_fut);
    let lookups_sent = lookups.load(Ordering::Relaxed);
    cache.stats.lookups += u64::from(lookups_sent);

    let (origin, origin_key_kind, ip_changed, error) = match fresh_origin {
        Some(loc) => {
            let raw_key = probed.clone().unwrap_or_else(|| loc.ip.clone());
            let (key, kind) = key_or_raw(&raw_key);
            cache.put(&key, location_to_cached(&loc, now, iface));
            cache.evict_oldest_beyond(LOCATION_CACHE_MAX_ENTRIES);
            // `ip_changed` 只在**探测成功**时才有意义（见字段文档）。
            (Some(loc), kind, probed.is_some(), None)
        }
        None => {
            let raw_key = probed.clone().unwrap_or_default();
            cache.last_failure = Some(LocationFailure {
                ip: raw_key.clone(),
                failed_unix: now,
                reason: "四个坐标源都没返回".to_string(),
            });
            // 有上次可用结果就先给出来（stale），别把用户放空手上。
            let fallback = if raw_key.is_empty() {
                None
            } else {
                let (key, kind) = key_or_raw(&raw_key);
                cache.get(&key).map(|e| (key, kind, e.clone()))
            };
            match fallback {
                Some((key, kind, entry)) => {
                    cache.stats.hits += 1;
                    cache.stats.stale_hits += 1;
                    let (info, origin, self_check) =
                        served_from_cache(&key, kind, &entry, now, true, false, lookups_sent);
                    return CallOutcome {
                        origin: Some(origin),
                        exit,
                        self_check,
                        cache: info,
                        error: None,
                        probe_sent,
                        lookups_sent,
                    };
                }
                None => (
                    None,
                    LocationKeyKind::Ip,
                    false,
                    Some("查本机位置失败（四个数据源都没返回）".to_string()),
                ),
            }
        }
    };

    let error = if error.is_some() {
        error
    } else if exit_ip.is_some() && exit.is_none() {
        Some("查节点位置失败（四个数据源都没返回）".to_string())
    } else {
        None
    };
    let self_check = SelfCheck::judge(iface, origin.as_ref());
    let cache_info = GlobeCacheInfo {
        from_cache: lookups_sent == 0,
        fetched_unix: None,
        ip_changed,
        stale: false,
        probe_cached: probe_sent == 0,
        key_kind: origin_key_kind.as_str().to_string(),
        age_s: None,
    };

    CallOutcome { origin, exit, self_check, cache: cache_info, error, probe_sent, lookups_sent }
}

/// 打一行**用户能 grep 出来自己算命中率**的日志（0.9.1-C / L8）。
///
/// 本仓库没有遥测；这一行 + 缓存文件里的 `stats` 是唯一的诚实度量方式。
/// 形如：
/// `location cache=hit probe=cached key=ip lookups=1 hits=4 stale_hits=0 calls=5 probe_calls=1`
fn log_location_cache(info: &GlobeCacheInfo, stats: &LocationStats, note: &str) {
    let cache = if info.stale {
        "stale"
    } else if info.from_cache {
        "hit"
    } else {
        "miss"
    };
    let probe = if info.probe_cached { "cached" } else { "fresh" };
    tracing::info!(
        cache = %cache,
        probe = %probe,
        key = %info.key_kind,
        lookups = stats.lookups,
        hits = stats.hits,
        stale_hits = stats.stale_hits,
        calls = stats.calls,
        probe_calls = stats.probe_calls,
        probe_cached = stats.probe_cached,
        note = %note,
        "location"
    );
}

/// 地球仪：本机 → 出口节点。
///
/// `force = true` 表示用户手动点了「刷新」：**跳过缓存与冷却**、强制完整查询。
#[tauri::command]
pub async fn globe_data(state: State<'_, AppState>, force: bool) -> Result<GlobeData, String> {
    let iface = physical_interface();

    // 当前选中的节点
    let node = state.with(|i| {
        let selected = i.settings.selected_node.clone();
        selected.and_then(|id| {
            i.nodes
                .iter()
                .find(|n| n.id == id)
                .map(|n| (n.name.clone(), n.address.clone(), n.outbound_tag()))
        })
    });
    let node = node.flatten();

    // **单飞**：与启动预热共用同一道闸门。第二个并发调用会等第一个把缓存写好，
    // 然后**重新读缓存**（TTL 记忆命中）⇒ 四源查询只跑一次。
    single_flight(|| globe_data_locked(&state, force, iface, node)).await
}

async fn globe_data_locked(
    state: &State<'_, AppState>,
    force: bool,
    iface: Option<String>,
    node: Option<(String, String, String)>,
) -> Result<GlobeData, String> {
    // 节点地址解析成 IP（域名取第一个）
    let exit_ip_str = node.as_ref().and_then(|(_, addr, _)| {
        xt_core::net::resolve_host(addr)
            .into_iter()
            .find(|ip| !is_private(*ip))
            .map(|ip| ip.to_string())
    });

    // 读失败（坏文件/版本不符）会退回空缓存并 warn，最多导致重查一次，不会让页面打不开。
    let mut cache = state.store.load_location_cache();
    let now = now_unix();
    let net = CurlNet { interface: iface.clone() };

    let outcome = run_call(
        &net,
        &mut cache,
        now,
        force,
        exit_ip_str.as_deref(),
        iface.as_deref(),
    )
    .await;

    // 统计、last_probe、last_failure 都要跨进程活着 ⇒ **每次调用都写回**
    // （它们正是「下次能不能省一次网络」的依据，只写在内存里等于没有）。
    if let Err(e) = state.store.save_location_cache(&cache) {
        // 写不进去只是「下次还得重查」，不该让本次结果失败。
        tracing::warn!(error = %e, "位置缓存写盘失败（下次仍会重查；本次结果不受影响）");
    }
    log_location_cache(
        &outcome.cache,
        &cache.stats,
        if force { "force" } else { "auto" },
    );

    // 实测流量：**只认该节点的 outbound tag**（task-179 / A20 —— 不再取最大）
    let route = match (outcome.origin.clone(), outcome.exit) {
        (Some(from), Some(to)) => {
            let node_tag = node.as_ref().map(|(_, _, tag)| tag.clone());
            let traffic = current_exit_traffic(node_tag.as_deref()).await;
            Some(GlobeRoute {
                from,
                to,
                bytes: traffic.bytes,
                traffic_ok: traffic.ok,
                counter_resets: traffic.resets,
                node_name: node.map(|(n, _, _)| n).unwrap_or_else(|| "节点".into()),
                traffic: traffic.provenance.clone(),
            })
        }
        _ => None,
    };

    Ok(GlobeData {
        route,
        origin: outcome.origin,
        error: outcome.error,
        self_check: outcome.self_check,
        cache: outcome.cache,
    })
}

/// 用系统 curl 发一次 GET。`interface` 为 `Some` 时绑到该网卡（绕过隧道）。
///
/// 用 curl 而不是自写 socket：https 需要 TLS，而项目已有既定做法
/// （见 `commands/nodes.rs` 的订阅拉取）—— 零依赖、走系统信任链。
async fn curl_get(url: &str, interface: Option<&str>) -> Option<String> {
    let mut cmd = tokio::process::Command::new("/usr/bin/curl");
    cmd.args([
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "8",
        "--user-agent",
        "XrayTun/location",
    ]);
    if let Some(iface) = interface {
        cmd.args(["--interface", iface]);
    }
    let out = cmd.arg(url).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

// ---------------------------------------------------------------------------
// 纯逻辑：解析 / 合并 / 缓存判定（无网络，全部可单测）
// ---------------------------------------------------------------------------

/// 两个小数位：`sources` 摘要里显示坐标用。
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 从若干候选里挑一个**便民**的字符串：优先非 ASCII（中文），其次第一个非空，
/// 最后退回 `fallback`。
///
/// 「中文优先」用的是**含非 ASCII 字符**这个判据（与具体服务无关），与
/// `xt_core::geo_lookup::merge_sources` 的口径一致：`ipwho.is` 的坐标更准但只有
/// 英文，`ip-api.com` 的 `lang=zh-CN` 才有「大理」。三个源里任何一个给出中文都算数。
fn prefer_chinese<'a>(values: impl Iterator<Item = &'a str>, fallback: &str) -> String {
    let candidates: Vec<&str> = values.filter(|v| !v.is_empty()).collect();
    if let Some(v) = candidates.iter().find(|v| !v.is_ascii()) {
        return (*v).to_string();
    }
    candidates
        .first()
        .map_or_else(|| fallback.to_string(), |v| (*v).to_string())
}

/// 两字母 ISO 国家/地区**代码**（`HK` / `CN`…）—— 它不是给人看的国家名。
fn is_country_code(value: &str) -> bool {
    let t = value.trim();
    t.len() == 2 && t.chars().all(|c| c.is_ascii_alphabetic())
}

/// 选**展示用的国家/地区名**：优先非 ASCII 全名（中文），其次任意非代码全名；
/// **绝不**把两字母代码当国家名输出。
///
/// # 为什么单独一个函数
///
/// `ipinfo.io` 的 `country` 是 `"HK"`，而 `ipwho.is`/`ifconfig.co` 给 `"Hong Kong"`、
/// `ip-api.com` 给 `"香港"`。如果只按「第一个非空」挑，四个源里只要前面几个没返回，
/// 界面就会显示 `HK` —— 那是**代码**，不是名称。所以这里把代码整类排除：
///
/// * 有任何全名 ⇒ 用全名（中文优先）；
/// * **四个源都只给了代码** ⇒ 返回**空串**（如实表示「没有可展示的名称」），
///   而不是把代码抄上去。空串在界面上就是不显示，比显示 `HK` 诚实。
fn prefer_country_name<'a>(values: impl Iterator<Item = &'a str>, fallback: &str) -> String {
    let names: Vec<&str> = values
        .map(str::trim)
        .filter(|v| !v.is_empty() && !is_country_code(v))
        .collect();
    if let Some(v) = names.iter().find(|v| !v.is_ascii()) {
        return (*v).to_string();
    }
    if let Some(v) = names.first() {
        return (*v).to_string();
    }
    // 全部候选都是代码：只在 fallback 本身是全名时才用它（同样不许漏代码）。
    if is_country_code(fallback) {
        String::new()
    } else {
        fallback.trim().to_string()
    }
}

/// 坐标是否「一致」：经纬度都在 [`CONSISTENT_TOLERANCE_DEG`] 之内。
fn coords_agree(a: &GeoLocation, b: &GeoLocation) -> bool {
    (a.lat - b.lat).abs() <= CONSISTENT_TOLERANCE_DEG
        && (a.lon - b.lon).abs() <= CONSISTENT_TOLERANCE_DEG
}

/// **任意数量**坐标源的并发互校（纯函数）：**多数一致**才 `consistent = true`。
///
/// # 与两源版 `merge_sources` 的关系
///
/// 两源版回答的是「两个都返回且坐标接近吗」；多源版还必须回答它没回答的情形：
/// **只有两个源返回、而这两个互相矛盾**、**两两各成一派（2-2 平局）**、
/// 以及更多源时的「有一派严格过半吗」。前者都不算一致：
///
/// | 情况 | `consistent` | 坐标取自 |
/// | --- | --- | --- |
/// | 全部一致 | `true` | 优先级最高者（`results` 顺序 = 优先级）|
/// | N-1（一派严格过半，如 5/8、3/4）| `true` | 多数派里优先级最高者 |
/// | **2-2 平局 / 4-4 平局** | **`false`**（没有多数派，**不默认挑一边**）| 优先级最高者，仅作占位 |
/// | 2-1-1（票没过半）| `false` | 优先级最高者 |
/// | 两家互相矛盾（其余没返回）| `false` | 优先级最高者 |
/// | 只有一家返回 | `false`（**没人印证，不是「一致」**）| 那一家 |
///
/// `consistent` 的判据有两条，缺一不可：
/// 1. 簇里**至少 2 家**（只有一家返回时「1 > 0」也成立，但没人印证）；
/// 2. 这簇**严格多于**剩下的源数（`support > total - support`）——
///    任何平局（2-2 / 4-4 / 2-1-1 的 2 vs 2）都不成立 ⇒ `false`。
///
/// 地名/国家/ISP 从**同一簇**里取：城市/ISP 优先中文（见 [`prefer_chinese`]），
/// 国家用 [`prefer_country_name`]（**绝不输出两字母代码**）。
/// `sources` **逐个列出全部源**（没返回的写「无结果」），界面据此说明问到了几家。
fn merge_many(ip: &str, results: &[(&'static str, Option<GeoLocation>)]) -> Option<GeoLocation> {
    let successes: Vec<(usize, GeoLocation)> = results
        .iter()
        .enumerate()
        .filter_map(|(idx, (_, r))| r.clone().map(|l| (idx, l)))
        .collect();
    if successes.is_empty() {
        return None;
    }

    // 每个成功源「与几个成功源（含自己）一致」；并列时取优先级更高（下标更小）的那家。
    let support: Vec<usize> = successes
        .iter()
        .map(|(_, a)| successes.iter().filter(|(_, b)| coords_agree(a, b)).count())
        .collect();
    let mut best = 0usize;
    for (i, s) in support.iter().enumerate().skip(1) {
        if *s > support[best] {
            best = i;
        }
    }
    let consistent = support[best] >= 2 && support[best] > successes.len() - support[best];
    let base = successes[best].1.clone();

    // 簇 = 与 base 一致的那些源（至少含 base 自己）。
    let cluster: Vec<&GeoLocation> = successes
        .iter()
        .filter(|(_, l)| coords_agree(l, &base))
        .map(|(_, l)| l)
        .collect();

    let city = prefer_chinese(cluster.iter().map(|l| l.city.as_str()), &base.city);
    // 国家单独走「不许代码」的那条路（ipinfo.io / geoiplookup.io / freeipapi 都给代码）。
    let country = prefer_country_name(cluster.iter().map(|l| l.country.as_str()), &base.country);
    let isp = prefer_chinese(cluster.iter().map(|l| l.isp.as_str()), &base.isp);

    let sources: Vec<String> = results
        .iter()
        .map(|(name, r)| match r {
            Some(l) => format!("{name}: {}", round2(l.lat)),
            None => format!("{name}: 无结果"),
        })
        .collect();

    Some(GeoLocation {
        // 调用方给的 IP（本机那次是**探测到的公网 IP**）优先：它是本次的缓存键。
        ip: if ip.is_empty() { base.ip.clone() } else { ip.to_string() },
        country,
        city,
        lat: base.lat,
        lon: base.lon,
        isp,
        source: base.source.clone(),
        consistent,
        sources,
    })
}

/// 解析 `ipinfo.io` 的 `/json` 响应。
///
/// ⚠️ **`country` 是两字母代码**（本机实测 `"HK"`），不能直接当展示用的国家名
/// （见 [`prefer_country_name`]）；坐标在 `loc` 里、是 `"lat,lon"` **字符串**。
/// `org` 形如 `"AS401701 cognetcloud INC"` —— 原样保留（AS 号对排障有用），
/// 不在解析层裁掉。
fn parse_ipinfo(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct IpInfo {
        #[serde(default)]
        ip: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        loc: String,
        #[serde(default)]
        org: String,
    }
    let p: IpInfo = serde_json::from_str(body).ok()?;
    // 成功响应一定带 ip；缺它说明拿到的不是这个 API 的 JSON（例如被挡后的错误页）。
    if p.ip.is_empty() {
        return None;
    }
    let (lat, lon) = parse_lat_lon_pair(&p.loc)?;
    Some(GeoLocation {
        ip: p.ip,
        country: p.country,
        city: p.city,
        lat,
        lon,
        isp: p.org,
        source: "ipinfo.io".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 拆 `loc` 里的 `"lat,lon"` 并**校验范围**（非数字/越界都算没查到）。
fn parse_lat_lon_pair(loc: &str) -> Option<(f64, f64)> {
    let (lat_s, lon_s) = loc.split_once(',')?;
    let lat: f64 = lat_s.trim().parse().ok()?;
    let lon: f64 = lon_s.trim().parse().ok()?;
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some((lat, lon))
}

/// 解析 `ifconfig.co` 的 `/json` 响应。
///
/// `country` 是全名（实测 `"Hong Kong"`），坐标是数字 `latitude`/`longitude`，
/// ISP 在 `asn_org`。同一个响应里还有 `country_iso`（两字母），**刻意不用** ——
/// 展示层要的是全名。
///
/// ⚠️ **两个坐标字段是 `Option<f64>`，不是 `f64`**：`/json?ip=<ip>` 那条路径实测
/// **根本不返回** `latitude`/`longitude`。用 `#[serde(default)] f64` 会把「缺失」
/// 读成 `0.0`，于是造出一个 **(0,0) 的假点**（几内亚湾），还会以这个位置参与
/// 四源坐标投票、把别的源判成「不一致」。缺失一律 `None` ⇒ 这一源记「无结果」。
fn parse_ifconfig_co(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct IfConfigCo {
        #[serde(default)]
        ip: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        latitude: Option<f64>,
        #[serde(default)]
        longitude: Option<f64>,
        #[serde(default)]
        asn_org: String,
    }
    let p: IfConfigCo = serde_json::from_str(body).ok()?;
    if p.ip.is_empty() {
        return None;
    }
    let (Some(lat), Some(lon)) = (p.latitude, p.longitude) else {
        return None;
    };
    if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }
    Some(GeoLocation {
        ip: p.ip,
        country: p.country,
        city: p.city,
        lat,
        lon,
        isp: p.asn_org,
        source: "ifconfig.co".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 坐标范围校验：越界一律当**没查到**（不 clamp —— 那会造出一个假点）。
fn valid_coords(lat: f64, lon: f64) -> Option<(f64, f64)> {
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// 解析 `ipwhois.app` 的响应（**与 `ipwho.is` 是两家**，字段形状不同）。
///
/// 实测形状：`{"ip":…,"success":true,"country":"Hong Kong","city":"Hong Kong",
/// "latitude":22.2783,"longitude":114.1747,…,"connection":{"isp":"Vapeline Technology"}}`。
fn parse_ipwhois_app(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct Who {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        ip: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        latitude: Option<f64>,
        #[serde(default)]
        longitude: Option<f64>,
        #[serde(default)]
        connection: Option<Conn>,
    }
    #[derive(Deserialize)]
    struct Conn {
        #[serde(default)]
        isp: String,
        #[serde(default)]
        org: String,
    }
    let p: Who = serde_json::from_str(body).ok()?;
    if !p.success || p.ip.is_empty() {
        return None;
    }
    let (lat, lon) = valid_coords(p.latitude?, p.longitude?)?;
    let isp = p.connection.map_or_else(String::new, |c| {
        if c.isp.is_empty() {
            c.org
        } else {
            c.isp
        }
    });
    Some(GeoLocation {
        ip: p.ip,
        country: p.country,
        city: p.city,
        lat,
        lon,
        isp,
        source: "ipwhois.app".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 解析 `json.geoiplookup.io` 的响应。
///
/// 实测形状：`{"ip":…,"isp":"Vapeline Technology","latitude":22.3193,"longitude":114.169,
/// "city":"Hong Kong","country_code":"HK","country_name":"Hong Kong",…}`。
/// 用 `country_name`（全名），**不用** `country_code`。
fn parse_geoiplookup(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct GeoIpLookup {
        #[serde(default)]
        ip: String,
        #[serde(default)]
        isp: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        country_name: String,
        #[serde(default)]
        latitude: Option<f64>,
        #[serde(default)]
        longitude: Option<f64>,
    }
    let p: GeoIpLookup = serde_json::from_str(body).ok()?;
    if p.ip.is_empty() {
        return None;
    }
    let (lat, lon) = valid_coords(p.latitude?, p.longitude?)?;
    Some(GeoLocation {
        ip: p.ip,
        country: p.country_name,
        city: p.city,
        lat,
        lon,
        isp: p.isp,
        source: "geoiplookup.io".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 解析 `freeipapi.com/api/json` 的响应（**会 307 重定向** ⇒ 必须走带 `--location`
/// 的 `curl_get`；自己另写请求会拿到 HTML）。
///
/// 实测形状：`{"ipAddress":…,"latitude":22.3193,"longitude":114.169,
/// "countryName":"Hong Kong","countryCode":"HK","cityName":"Hong Kong",…}`。
fn parse_freeipapi(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct FreeIpApi {
        #[serde(default)]
        ip_address: String,
        #[serde(default)]
        city_name: String,
        #[serde(default)]
        country_name: String,
        #[serde(default)]
        latitude: Option<f64>,
        #[serde(default)]
        longitude: Option<f64>,
    }
    let p: FreeIpApi = serde_json::from_str(body).ok()?;
    if p.ip_address.is_empty() {
        return None;
    }
    let (lat, lon) = valid_coords(p.latitude?, p.longitude?)?;
    Some(GeoLocation {
        ip: p.ip_address,
        country: p.country_name,
        city: p.city_name,
        lat,
        lon,
        isp: String::new(),
        source: "freeipapi.com".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 解析 `api.ipquery.io` 的响应。
///
/// ⚠️ 坐标在**嵌套的 `location` 里**，不是顶层：
/// `{"ip":…,"isp":{"isp":…},"location":{"country":"Hong Kong","city":"Hong Kong",
/// "latitude":22.30,"longitude":114.17,…}}`。
fn parse_ipquery(body: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct IpQuery {
        #[serde(default)]
        ip: String,
        #[serde(default)]
        isp: Option<Isp>,
        #[serde(default)]
        location: Option<Loc>,
    }
    #[derive(Deserialize)]
    struct Isp {
        #[serde(default)]
        isp: String,
        #[serde(default)]
        org: String,
    }
    #[derive(Deserialize)]
    struct Loc {
        #[serde(default)]
        country: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        latitude: Option<f64>,
        #[serde(default)]
        longitude: Option<f64>,
    }
    let p: IpQuery = serde_json::from_str(body).ok()?;
    if p.ip.is_empty() {
        return None;
    }
    let loc = p.location?;
    let (lat, lon) = valid_coords(loc.latitude?, loc.longitude?)?;
    let isp = p.isp.map_or_else(String::new, |i| {
        if i.isp.is_empty() {
            i.org
        } else {
            i.isp
        }
    });
    Some(GeoLocation {
        ip: p.ip,
        country: loc.country,
        city: loc.city,
        lat,
        lon,
        isp,
        source: "ipquery.io".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// `api.ipify.org` 返回**纯文本 IP**。
fn parse_ipify(body: &str) -> Option<String> {
    body.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// `1.1.1.1/cdn-cgi/trace` 是多行 `k=v`，其中一行是 `ip=<IPv4|IPv6>`。
fn parse_cf_trace(body: &str) -> Option<String> {
    let value = body.lines().find_map(|line| line.trim().strip_prefix("ip="))?;
    value.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// `api.bigdatacloud.net/data/client-ip` → `{"ipString":"1.2.3.4","ipType":"IPv4"}`。
fn parse_bigdatacloud(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct BigDataCloud {
        #[serde(default)]
        ip_string: String,
    }
    let p: BigDataCloud = serde_json::from_str(body).ok()?;
    p.ip_string.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// `api.country.is/` → `{"ip":"1.2.3.4","country":"HK"}`（只用它的 `ip`）。
fn parse_country_is(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct CountryIs {
        #[serde(default)]
        ip: String,
    }
    let p: CountryIs = serde_json::from_str(body).ok()?;
    p.ip.trim().parse::<IpAddr>().ok().map(|ip| ip.to_string())
}

/// 四个 IP-only 源里任一成功即可判定。
///
/// 优先级**按加入顺序**（先有的先用，保持结果确定）：
/// `ipify` → `1.1.1.1/cdn-cgi/trace` → `bigdatacloud` → `country.is`。
/// 它们都只回答一件事（"我的公网 IP 是什么"），所以优先级不影响正确性，
/// 只影响"两个源都答了且不一致时信谁"——信先验更稳的那个。
fn pick_public_ip(
    ipify: Option<&str>,
    cloudflare: Option<&str>,
    bigdatacloud: Option<&str>,
    country_is: Option<&str>,
) -> Option<String> {
    ipify
        .and_then(parse_ipify)
        .or_else(|| cloudflare.and_then(parse_cf_trace))
        .or_else(|| bigdatacloud.and_then(parse_bigdatacloud))
        .or_else(|| country_is.and_then(parse_country_is))
}

// 注：0.9.1-C 起「IP 变了没有」不再是独立函数 —— 它现在是 `run_call` 里
// 「探测成功但缓存没命中」这一条路径的**结论**（`ip_changed = probed.is_some()`），
// 而探测失败那一条走的是 `stale=true`（不再把「没测到」说成「变了」）。

/// 查到的新结果 → 缓存条目。
fn location_to_cached(
    loc: &GeoLocation,
    fetched_unix: u64,
    bound_interface: Option<&str>,
) -> CachedLocation {
    CachedLocation {
        country: loc.country.clone(),
        city: loc.city.clone(),
        lat: loc.lat,
        lon: loc.lon,
        isp: loc.isp.clone(),
        source: loc.source.clone(),
        consistent: loc.consistent,
        sources: loc.sources.clone(),
        fetched_unix,
        bound_interface: bound_interface.map(str::to_string),
    }
}

/// 缓存条目 → 对外的位置（`ip` 用键；缓存里不存 IP 本身）。
fn cached_to_location(ip: &str, entry: &CachedLocation) -> GeoLocation {
    GeoLocation {
        ip: ip.to_string(),
        country: entry.country.clone(),
        city: entry.city.clone(),
        lat: entry.lat,
        lon: entry.lon,
        isp: entry.isp.clone(),
        source: entry.source.clone(),
        consistent: entry.consistent,
        sources: entry.sources.clone(),
    }
}

/// 缓存命中时的「本机」可信性判定（task-179 / A21）。
///
/// **不许拿「本次绑了网卡」给一条旧记录背书**：可信性属于**抓取那一刻的查询**。
/// 所以这里读条目里存的 `bound_interface`；当时没绑卡 ⇒ 照旧 `trusted = false`
/// 并给出**针对缓存**的具体原因（而不是 `SelfCheck::judge` 里那句「读不到物理
/// 默认路由」——那句话描述的不是这个场景）。
fn self_check_from_cache(entry: &CachedLocation, origin: &GeoLocation) -> SelfCheck {
    match entry.bound_interface.as_deref() {
        Some(iface) => SelfCheck::judge(Some(iface), Some(origin)),
        None => SelfCheck {
            ip: Some(origin.ip.clone()),
            bound_interface: None,
            trusted: false,
            reason: Some(
                "这条位置来自缓存，而缓存它时没绑定物理网卡 ⇒ 无法确认它说的是本机\
                 （隧道开着时可能是节点出口）。点刷新可重新验证"
                    .to_string(),
            ),
        },
    }
}

// ---------------------------------------------------------------------------
// 网络：全部走系统 curl（零依赖、走系统信任链、可 `--interface` 绑物理网卡）
// ---------------------------------------------------------------------------

/// 轻量公网 IP 探测：**只取 IP，不取坐标**。
///
/// **四个**源并发，任一成功即可（优先级见 [`pick_public_ip`]）。它只回答一个问题
/// ——「公网 IP 变了没有」—— 所以比坐标查询便宜，也不该在 IP 没变时触发坐标查询。
/// 四路并发**不叠加**延迟：单次上限仍是 `curl_get` 里的 `--max-time 8`。
async fn query_public_ip(interface: Option<&str>) -> Option<String> {
    let (ipify, cloudflare, bigdatacloud, country_is) = tokio::join!(
        curl_get("https://api.ipify.org", interface),
        curl_get("https://1.1.1.1/cdn-cgi/trace", interface),
        curl_get("https://api.bigdatacloud.net/data/client-ip", interface),
        curl_get("https://api.country.is/", interface),
    );
    pick_public_ip(
        ipify.as_deref(),
        cloudflare.as_deref(),
        bigdatacloud.as_deref(),
        country_is.as_deref(),
    )
}

/// 查一个指定 IP 的位置（**八源**互校）。
///
/// ⚠️ 实测：`https://ifconfig.co/json?ip=<ip>` **只给国家与 ASN，不给经纬度/城市**
/// （`curl -sS 'https://ifconfig.co/json?ip=1.1.1.1'` ⇒ 没有 `latitude`/`longitude`）。
/// 所以查**节点**时它在 `sources` 里会如实记成「无结果」，且**绝不会**被当成 (0,0)
/// 参与坐标投票（见 [`parse_ifconfig_co`] 的 `Option` 校验）。仍然问它：上游一旦
/// 补上坐标就自动生效，而且 `sources` 里必须能看到「这一家问过了」。
///
/// 八个源**全部并发**（`tokio::join!`）—— 顺序 await 会让最坏延迟线性叠加。
/// `tokio::join!` 支持任意数量的 future，8 个不是问题。
async fn query_ip(ip: &str, interface: Option<&str>) -> Option<GeoLocation> {
    let (who_url, api_url, info_url, ifc_url) = (
        format!("https://ipwho.is/{ip}"),
        format!("http://ip-api.com/json/{ip}?fields=status,message,country,city,lat,lon,isp&lang=zh-CN"),
        format!("https://ipinfo.io/{ip}/json"),
        format!("https://ifconfig.co/json?ip={ip}"),
    );
    let (whoisapp_url, geoip_url, freeip_url, ipquery_url) = (
        format!("https://ipwhois.app/json/{ip}"),
        format!("https://json.geoiplookup.io/{ip}"),
        format!("https://freeipapi.com/api/json/{ip}"),
        format!("https://api.ipquery.io/{ip}?format=json"),
    );
    let (who, api, info, ifc, whoisapp, geoip, freeip, ipquery) = tokio::join!(
        curl_get(&who_url, interface),
        curl_get(&api_url, interface),
        curl_get(&info_url, interface),
        curl_get(&ifc_url, interface),
        curl_get(&whoisapp_url, interface),
        curl_get(&geoip_url, interface),
        curl_get(&freeip_url, interface),
        curl_get(&ipquery_url, interface),
    );
    merge_many(
        ip,
        &[
            ("ipwho.is", who.and_then(|b| parse_who(&b, ip))),
            ("ip-api.com", api.and_then(|b| parse_api(&b, ip))),
            ("ipinfo.io", info.and_then(|b| parse_ipinfo(&b))),
            ("ifconfig.co", ifc.and_then(|b| parse_ifconfig_co(&b))),
            ("ipwhois.app", whoisapp.and_then(|b| parse_ipwhois_app(&b))),
            ("geoiplookup.io", geoip.and_then(|b| parse_geoiplookup(&b))),
            ("freeipapi.com", freeip.and_then(|b| parse_freeipapi(&b))),
            ("ipquery.io", ipquery.and_then(|b| parse_ipquery(&b))),
        ],
    )
}

/// 查**本机**的公网位置（**八源**互校）。服务自己看到的是发起请求的出口地址，
/// 所以 url 里不带 IP；`probed_ip` 只作为「源没给出 IP 时」的兜底与缓存键。
async fn query_self(
    interface: Option<&str>,
    probed_ip: Option<&str>,
) -> Option<GeoLocation> {
    let fallback = probed_ip.unwrap_or("");
    let (who, api, info, ifc, whoisapp, geoip, freeip, ipquery) = tokio::join!(
        curl_get("https://ipwho.is/", interface),
        curl_get(
            "http://ip-api.com/json/?fields=status,message,country,city,lat,lon,isp,query&lang=zh-CN",
            interface,
        ),
        curl_get("https://ipinfo.io/json", interface),
        curl_get("https://ifconfig.co/json", interface),
        curl_get("https://ipwhois.app/json/", interface),
        curl_get("https://json.geoiplookup.io/", interface),
        // ⚠️ freeipapi 会 **307 重定向**：`curl_get` 带 `--location`，所以这里没问题；
        // 自己另写请求会拿到 HTML（解析器会如实返回 `None`）。
        curl_get("https://freeipapi.com/api/json", interface),
        curl_get("https://api.ipquery.io/?format=json", interface),
    );
    merge_many(
        fallback,
        &[
            ("ipwho.is", who.and_then(|b| parse_who(&b, fallback))),
            ("ip-api.com", api.and_then(|b| parse_api(&b, fallback))),
            ("ipinfo.io", info.and_then(|b| parse_ipinfo(&b))),
            ("ifconfig.co", ifc.and_then(|b| parse_ifconfig_co(&b))),
            ("ipwhois.app", whoisapp.and_then(|b| parse_ipwhois_app(&b))),
            ("geoiplookup.io", geoip.and_then(|b| parse_geoiplookup(&b))),
            ("freeipapi.com", freeip.and_then(|b| parse_freeipapi(&b))),
            ("ipquery.io", ipquery.and_then(|b| parse_ipquery(&b))),
        ],
    )
}

/// App 启动时的**后台预热**：把当前公网 IP 的位置提前拉进缓存，
/// 用户点开「位置」页就是瞬时的。
///
/// # 它必须是非阻塞的
///
/// 由 `lib.rs` 的 `setup` 用 `tauri::async_runtime::spawn` 调起（**不 await**）——
/// 用户不该为一次第三方地理查询等启动。失败**只 log**，绝不写 `last_notice`：
/// 那不是用户此刻做的动作，为它弹错就是「狼来了」。
///
/// # 什么时候跳过
///
/// 走的是与页面**同一套决策**（[`run_call`]）：短 TTL 记忆命中、或失败还在冷却里，
/// 它一个请求都不发。也就是说「已经预热好了」由缓存状态自己表达，不另设判据。
pub async fn prewarm_location_cache(app: AppHandle) {
    let Some(state) = app.try_state::<AppState>() else {
        tracing::warn!("位置预热：应用状态不可用，跳过");
        return;
    };
    let iface = physical_interface();
    // **与页面调用同一道单飞闸门**：两者同时发生时只跑一次四源查询。
    single_flight(|| async {
        let now = now_unix();
        let mut cache = state.store.load_location_cache();
        let net = CurlNet { interface: iface.clone() };
        // 预热只关心**本机位置**（页面的首屏就是它）；出口节点位置不在这里查。
        let outcome = run_call(&net, &mut cache, now, false, None, iface.as_deref()).await;
        if let Err(e) = state.store.save_location_cache(&cache) {
            tracing::warn!(error = %e, "位置预热：缓存写盘失败（下次进页面仍会重查）");
        }
        log_location_cache(&outcome.cache, &cache.stats, "prewarm");
        if outcome.origin.is_some() {
            tracing::info!(
                probe_sent = outcome.probe_sent,
                lookups_sent = outcome.lookups_sent,
                "位置预热完成：点开「位置」页应当是瞬时的"
            );
        } else {
            tracing::warn!(
                error = ?outcome.error,
                "位置预热：没能拿到本机位置（只 log、不弹错）；下次进「位置」页会再试"
            );
        }
    })
    .await;
}

/// 出口累计字节的读取结果：把「值」与「这次是否查到」分开表达。
///
/// **不用 0 表示「没查到」**：0 是合法读数（真的没有流量），混在一起会让
/// 地球仪在查询失败时显示「出口累计 0 B」——那是假读数。
#[derive(Debug, Clone, PartialEq, Eq)]
struct ExitTraffic {
    bytes: u64,
    ok: bool,
    /// 观察到的计数器归零次数（核心重启）。
    resets: u32,
    /// 这个数字的**归属**（task-179 / A20）：来自哪个 tag、可不可信。
    provenance: TrafficProvenance,
}

impl ExitTraffic {
    /// 这次没查到 / 归不到：`bytes` 是占位 0、`provenance` 明确「未归属 + 原因」。
    fn unattributed(reason: impl Into<String>) -> Self {
        Self {
            bytes: 0,
            ok: false,
            resets: 0,
            provenance: TrafficProvenance::unattributed(reason),
        }
    }
}

/// 从一次采样里算**该节点出站**的累计字节 —— 纯函数，便于测试
/// （这一段的错法都是「数字悄悄不对」）。
///
/// # task-179 / A20：**不再用「取最大」当归属**
///
/// 旧实现取「所有出站里 up+down 最大的那个」，注释写着「最大的那个就是节点出站」
/// —— 那是**假设**：真凶可能是 `direct`，用户却会得出「我的节点扛了 8 GiB」。
/// 现在只认**具体 tag**（`node-<id>`）；拿不到就给「未归属」+ 具体原因。
/// 累计值跨核心重启保持单调，见 [`MonotonicCounters`]。
fn read_exit_traffic(
    stats: Option<&[StatEntry]>,
    counters: &mut MonotonicCounters,
    node_tag: Option<&str>,
) -> ExitTraffic {
    let Some(node_tag) = node_tag else {
        return ExitTraffic::unattributed("没有选中的节点 ⇒ 归不到任何 outbound（不挑「最大的」顶上）");
    };
    let Some(stats) = stats else {
        return ExitTraffic::unattributed("查统计失败（核心没在跑或 API 不可达）⇒ 归属未验证");
    };
    let (by_tag, resets) = monotonic_traffic_by_tag(counters, stats, "outbound");
    // **只认这个 tag**：别的出站（例如 direct）流量再大也不算它的
    let (up, down) = by_tag.get(node_tag).copied().unwrap_or((0, 0));
    ExitTraffic {
        bytes: up.saturating_add(down),
        ok: true,
        resets,
        provenance: TrafficProvenance::verified_for(node_tag),
    }
}

/// 进程内的出口累计读数表：把「核心重启后计数器归零」补偿掉，见 topology.rs。
static EXIT_COUNTERS: OnceLock<Mutex<MonotonicCounters>> = OnceLock::new();

/// 当前出口节点的累计字节。查不到时 `ok=false`（而不是把 0 当读数）。
///
/// `node_tag` 来自选中节点的 [`xt_core::model::Node::outbound_tag`]；拿不到就如实「未归属」。
///
/// 锁包住查询：单调化依赖「采样顺序 = 观察顺序」，理由同 topology.rs。
async fn current_exit_traffic(node_tag: Option<&str>) -> ExitTraffic {
    let addr: std::net::SocketAddr =
        ([127, 0, 0, 1], xt_core::xray::config::API_PORT).into();
    let mut counters = EXIT_COUNTERS
        .get_or_init(|| Mutex::new(MonotonicCounters::new()))
        .lock()
        .await;
    let stats = xt_core::xray::query_stats(addr, Duration::from_millis(900))
        .await
        .ok();
    read_exit_traffic(stats.as_deref(), &mut counters, node_tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **真实网络**的双源查询验证。
    ///
    /// 默认不跑（CI 不该依赖外网），需要时手动：
    ///
    /// ```bash
    /// cargo test -p xraytun-desktop --lib globe -- --ignored --nocapture
    /// ```
    ///
    /// 它存在的价值：parse/merge 的单测证明不了「curl 那条路真的通」——
    /// 绑网卡的参数、url 形状、两个源的实际响应字段，只有真发一次才知道。
    #[tokio::test]
    #[ignore = "需要网络"]
    async fn real_lookup_returns_a_plausible_location() {
        let iface = physical_interface();
        println!("网卡 = {iface:?}");

        let origin = query_self(iface.as_deref(), None).await.expect("本机位置应当查得到");
        println!(
            "本机: {} {} ({:.4}, {:.4}) 来源={} 一致={} {:?}",
            origin.city, origin.country, origin.lat, origin.lon,
            origin.source, origin.consistent, origin.sources
        );
        assert!(!origin.ip.is_empty(), "应当拿到公网 IP");
        assert!(origin.lat.abs() <= 90.0 && origin.lon.abs() <= 180.0);
        // 三源都要出现在摘要里；没返回的源写成「无结果」（第三方限流是常态，
        // 所以这里只断言**三家都被点到**，不断言三家都成功）。
        assert_eq!(origin.sources.len(), 3, "三个源都要列出来（含「无结果」）");
        assert!(
            origin.sources.iter().any(|s| !s.contains("无结果")),
            "至少一家要真的返回：{:?}",
            origin.sources
        );

        let node = query_ip("45.207.197.185", iface.as_deref()).await.expect("节点位置应当查得到");
        println!(
            "节点: {} {} ({:.4}, {:.4}) 来源={} 一致={}",
            node.city, node.country, node.lat, node.lon, node.source, node.consistent
        );
        // 节点在香港（实测），纬度应当在 22 附近
        assert!((node.lat - 22.3).abs() < 2.0, "节点纬度应当在香港附近，实际 {}", node.lat);
    }

    /// 查不到流量时必须标记为不可用：把 0 当实测值会让地球仪显示
    /// 「出口累计 0 B」——那是一次查询失败，不是真的没有流量。
    #[test]
    fn unavailable_stats_are_flagged_rather_than_zero() {
        let t = read_exit_traffic(None, &mut MonotonicCounters::new(), Some("node-a"));
        assert!(!t.ok, "查不到必须标记为不可用");
        assert_eq!(t.bytes, 0);
        assert_eq!(t.resets, 0);
        assert!(!t.provenance.verified, "查不到时归属也不许标成「已验证」");
        assert!(
            t.provenance.reason.as_deref().is_some_and(|r| !r.is_empty()),
            "未验证必须给具体原因：{:?}",
            t.provenance
        );
    }

    /// **primary 回归**：核心重启让计数器归零，地球仪的累计字节不得回退。
    #[test]
    fn exit_bytes_do_not_regress_across_a_core_restart() {
        let mut counters = MonotonicCounters::new();
        let big = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 8_600_000_000,
        }];
        let first = read_exit_traffic(Some(&big), &mut counters, Some("node-a"));
        assert!(first.ok);
        assert_eq!(first.bytes, 8_600_000_000);
        assert_eq!(first.resets, 0);

        // 核心重启：计数器从 0 重新计
        let reset = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 0,
        }];
        let second = read_exit_traffic(Some(&reset), &mut counters, Some("node-a"));
        assert!(second.ok);
        assert_eq!(second.bytes, 8_600_000_000, "重启不得让累计值归零");
        assert_eq!(second.resets, 1);

        // 重启后的新流量继续累加
        let grown = vec![StatEntry {
            name: "outbound>>>node-a>>>traffic>>>downlink".into(),
            value: 1_024,
        }];
        assert_eq!(
            read_exit_traffic(Some(&grown), &mut counters, Some("node-a")).bytes,
            8_600_001_024
        );
    }

    /// **task-179 / A20 主回归**：归属**只认具体 tag**，不是「上下行合计最大的那个」。
    ///
    /// 构造一个「`direct` 明显比节点出站大」的世界：旧实现（取最大）会报 direct 的
    /// 18,000，新实现必须报**节点 tag 的** 1,800（并说明 tag 与是否节点出站）。
    #[test]
    fn attribution_follows_the_node_tag_not_the_busiest_outbound() {
        let stats = vec![
            // direct 更大 —— 真凶可能是它，但**不许**因此算到节点头上
            StatEntry { name: "outbound>>>direct>>>traffic>>>downlink".into(), value: 9_000 },
            StatEntry { name: "outbound>>>direct>>>traffic>>>uplink".into(), value: 9_000 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>downlink".into(), value: 900 },
            StatEntry { name: "outbound>>>node-a>>>traffic>>>uplink".into(), value: 900 },
            // 畸形：不参与
            StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 },
            // 入站计数不能混进出口
            StatEntry { name: "inbound>>>tun>>>traffic>>>downlink".into(), value: 10_000 },
        ];
        let t = read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), Some("node-a"));
        assert!(t.ok);
        assert_eq!(t.bytes, 1_800, "必须报**节点 tag** 的合计，而不是最大的 direct");
        assert_eq!(t.provenance.tag.as_deref(), Some("node-a"));
        assert!(t.provenance.verified, "拿到了具体 tag ⇒ 归属已验证");
        assert!(t.provenance.is_node_outbound, "`node-*` 是节点出站");
        assert!(t.provenance.reason.is_none(), "已验证时不该有原因");

        // 查到统计、但该 tag 没有计数 = 真的 0（不是「没查到」），且**归属仍是该 tag**
        let junk = vec![StatEntry { name: "outbound>>>node-a>>>traffic".into(), value: 7 }];
        let none = read_exit_traffic(Some(&junk), &mut MonotonicCounters::new(), Some("node-a"));
        assert!(none.ok, "查到统计时 ok 必须为真");
        assert_eq!(none.bytes, 0);
        assert_eq!(none.provenance.tag.as_deref(), Some("node-a"));
        assert!(none.provenance.verified);

        // 如果归属真的是 `direct`（例如直连规则在跑），**不许**把 `direct` 说成节点出站
        let direct =
            read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), Some("direct"));
        assert_eq!(direct.bytes, 18_000, "tag=direct 时报的就是 direct 的数字");
        assert_eq!(direct.provenance.tag.as_deref(), Some("direct"));
        assert!(!direct.provenance.is_node_outbound, "`direct` 不是节点出站");
        assert!(direct.provenance.verified, "拿到了具体 tag 仍算已验证归属");
    }

    /// **归不到 tag ⇒ 明确 unattributed**（不许挑一个顶上、也不许报数字）。
    #[test]
    fn without_a_node_tag_traffic_is_explicitly_unattributed() {
        let stats = vec![StatEntry {
            name: "outbound>>>direct>>>traffic>>>downlink".into(),
            value: 9_000,
        }];
        let t = read_exit_traffic(Some(&stats), &mut MonotonicCounters::new(), None);
        assert!(!t.ok);
        assert_eq!(t.bytes, 0, "未归属时不许报任何数字（哪怕是最大的那个）");
        assert!(!t.provenance.verified);
        assert_eq!(t.provenance.tag, None);
        assert!(!t.provenance.is_node_outbound);
        assert!(t
            .provenance
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("节点")));
    }

    // -----------------------------------------------------------------------
    // task-179 / A21：`SelfCheck` 的「字段 = X ⇒ 呈现 = Y」后端侧断言
    // -----------------------------------------------------------------------

    fn loc(ip: &str) -> GeoLocation {
        GeoLocation {
            ip: ip.into(),
            country: "中国".into(),
            city: "大理".into(),
            lat: 25.6,
            lon: 100.2,
            isp: "电信".into(),
            source: "ipwho.is".into(),
            consistent: true,
            sources: vec![],
        }
    }

    /// 只有「**绑了物理网卡 + 拿到了位置**」才可信；任何一边缺 ⇒ `trusted=false` 且原因具体。
    #[test]
    fn self_check_is_trusted_only_when_the_query_was_bound_and_answered() {
        let trusted = SelfCheck::judge(Some("en0"), Some(&loc("203.0.113.10")));
        assert!(trusted.trusted);
        assert_eq!(trusted.bound_interface.as_deref(), Some("en0"));
        assert_eq!(trusted.ip.as_deref(), Some("203.0.113.10"));
        assert!(trusted.reason.is_none());

        // 读不到物理网卡 ⇒ 走系统默认路由；隧道开着时查到的是**节点出口** ⇒ 不许说「本机」
        let unbound = SelfCheck::judge(None, Some(&loc("203.0.113.10")));
        assert!(!unbound.trusted, "没绑卡时查到的可能是节点出口，不许说「本机」");
        assert_eq!(unbound.bound_interface, None);
        assert!(unbound
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("节点出口")));

        // 绑了却没答 ⇒ 未验证（原因里点名是哪张网卡）
        let no_answer = SelfCheck::judge(Some("en0"), None);
        assert!(!no_answer.trusted);
        assert!(no_answer
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty() && r.contains("en0")));

        let nothing = SelfCheck::judge(None, None);
        assert!(!nothing.trusted && nothing.ip.is_none());
        assert!(nothing.reason.as_deref().is_some_and(|r| !r.is_empty()));
    }

    /// **不变量**（界面据此判「能不能说『本机』」）：
    /// `trusted = true` ⇔ 「绑了网卡 && 有 IP && 没写原因」；`trusted = false` ⇒ 原因非空。
    /// 任何一边破掉，这条测试就红 —— 双向敏感性就钉在这里。
    #[test]
    fn trusted_iff_bound_interface_and_ip_and_no_reason() {
        for (iface, origin) in [
            (Some("en0"), Some(loc("203.0.113.10"))),
            (None, Some(loc("203.0.113.10"))),
            (Some("en0"), None),
            (None, None),
        ] {
            let sc = SelfCheck::judge(iface, origin.as_ref());
            assert_eq!(
                sc.trusted,
                sc.bound_interface.is_some() && sc.ip.is_some() && sc.reason.is_none(),
                "trusted 必须等价于「绑了网卡 + 有 IP + 没原因」：{sc:?}"
            );
            if !sc.trusted {
                assert!(
                    sc.reason.as_deref().is_some_and(|r| !r.is_empty()),
                    "`trusted=false` 必须给具体原因（不许留空）：{sc:?}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // 0.9.1：三源合并 / 公网 IP 探测 / 缓存判定（全部无网络）
    // -----------------------------------------------------------------------

    /// 造一个指定源与坐标的位置。
    fn at(source: &str, city: &str, country: &str, lat: f64, lon: f64) -> GeoLocation {
        GeoLocation {
            ip: "1.2.3.4".into(),
            country: country.into(),
            city: city.into(),
            lat,
            lon,
            isp: "电信".into(),
            source: source.into(),
            consistent: true,
            sources: Vec::new(),
        }
    }

    fn entry(bound: Option<&str>) -> CachedLocation {
        location_to_cached(&at("ipwho.is", "Dali", "China", 25.6, 100.2), 1_790_000_000, bound)
    }

    /// **四家一致** ⇒ `consistent = true`；坐标取优先级最高的 `ipwho.is`，
    /// 中文地名/国家名优先（ip-api 的 `lang=zh-CN`），且最终国家不是两字母代码。
    #[test]
    fn four_sources_all_agree() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali Baizu", "China", 25.60, 100.26))),
                ("ip-api.com", Some(at("ip-api.com", "大理", "中国", 25.69, 100.16))),
                ("ipinfo.io", Some(at("ipinfo.io", "Dali", "CN", 25.61, 100.25))),
                ("ifconfig.co", Some(at("ifconfig.co", "Dali", "China", 25.60, 100.27))),
            ],
        )
        .expect("四家都返回");

        assert!(merged.consistent, "四家指向同一片坐标");
        assert!(
            (merged.lat - 25.60).abs() < 1e-9,
            "坐标取优先级最高的 ipwho.is，实际 {}",
            merged.lat
        );
        assert_eq!(merged.city, "大理", "中文地名优先");
        assert_eq!(merged.country, "中国", "中文全名优先于 ipinfo.io 的 `CN`");
        assert_eq!(merged.source, "ipwho.is");
        assert_eq!(merged.sources.len(), 4, "四个源都要列出（界面据此说明问了几家）");
    }

    /// **3-1**：三家一派 ⇒ 多数派成立（3/4 严格过半）；跑偏那家只留痕、不左右结果。
    #[test]
    fn four_sources_three_against_one() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali", "China", 25.60, 100.20))),
                ("ip-api.com", Some(at("ip-api.com", "大理", "中国", 25.69, 100.16))),
                ("ipinfo.io", Some(at("ipinfo.io", "Dali", "CN", 25.61, 100.25))),
                ("ifconfig.co", Some(at("ifconfig.co", "Guangzhou", "China", 23.13, 113.26))),
            ],
        )
        .expect("四家都返回");
        assert!(merged.consistent, "3/4 是严格多数");
        assert!((merged.lat - 25.60).abs() < 1e-9, "取多数派里优先级最高的");
        assert_eq!(merged.sources.len(), 4);
        assert!(
            merged.sources.iter().any(|s| s.contains("ifconfig.co")),
            "跑偏那家也要留痕：{:?}",
            merged.sources
        );
    }

    /// **2-2 平局** ⇒ **必须 `consistent = false`**：两派都不是多数，不许默认挑一边。
    #[test]
    fn four_sources_two_two_tie_is_not_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali", "China", 25.60, 100.20))),
                ("ip-api.com", Some(at("ip-api.com", "大理", "中国", 25.61, 100.21))),
                ("ipinfo.io", Some(at("ipinfo.io", "Guangzhou", "CN", 23.13, 113.26))),
                ("ifconfig.co", Some(at("ifconfig.co", "Guangzhou", "China", 23.14, 113.27))),
            ],
        )
        .expect("四家都返回");
        assert!(
            !merged.consistent,
            "2-2 平局没有多数派 ⇒ 必须判不一致（不许默认挑一边）"
        );
        assert!(
            (merged.lat - 25.60).abs() < 1e-9,
            "平局时只按优先级取占位坐标（同时如实标不一致）"
        );
        assert_eq!(merged.sources.len(), 4, "两派的值都要能看到");
    }

    /// **2-1-1**（四家都返回、只有两家一致）⇒ 2 票没过半 ⇒ 同样不一致。
    #[test]
    fn four_sources_plurality_without_majority_is_not_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali", "China", 25.60, 100.20))),
                ("ip-api.com", Some(at("ip-api.com", "大理", "中国", 25.61, 100.21))),
                ("ipinfo.io", Some(at("ipinfo.io", "Guangzhou", "CN", 23.13, 113.26))),
                ("ifconfig.co", Some(at("ifconfig.co", "Shanghai", "China", 31.23, 121.47))),
            ],
        )
        .expect("四家都返回");
        assert!(!merged.consistent, "2/4 不是严格多数");
    }

    // -----------------------------------------------------------------------
    // 0.9.1-E：8 个坐标源（5 一致 / 4-4 平局 / 只有 2 家 / 全无）
    // -----------------------------------------------------------------------

    /// 八个源里 **5 家一致** ⇒ 5 > 3 ⇒ 严格过半 ⇒ `consistent = true`。
    #[test]
    fn five_of_eight_agree_is_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("s1", Some(at("s1", "Dali", "China", 25.60, 100.20))),
                ("s2", Some(at("s2", "大理", "中国", 25.61, 100.21))),
                ("s3", Some(at("s3", "Dali", "CN", 25.62, 100.22))),
                ("s4", Some(at("s4", "Dali", "China", 25.59, 100.19))),
                ("s5", Some(at("s5", "Dali", "China", 25.60, 100.18))),
                ("s6", Some(at("s6", "Sydney", "Australia", -33.86, 151.20))),
                ("s7", Some(at("s7", "Tokyo", "Japan", 35.68, 139.69))),
                ("s8", None),
            ],
        )
        .expect("至少一家返回");
        assert!(merged.consistent, "5/8 严格过半");
        assert_eq!(merged.sources.len(), 8, "八个源都要列出（含「无结果」）");
        assert!((merged.lat - 25.60).abs() < 1e-9, "取多数派里优先级最高的");
        assert_eq!(merged.country, "中国", "中文全名仍优先（s3 给的是 CN 代码）");
    }

    /// **4-4 平局** ⇒ 没有多数派 ⇒ `consistent = false`（不许默认挑一边）。
    #[test]
    fn four_four_tie_is_not_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("s1", Some(at("s1", "Dali", "China", 25.60, 100.20))),
                ("s2", Some(at("s2", "大理", "中国", 25.61, 100.21))),
                ("s3", Some(at("s3", "Dali", "CN", 25.62, 100.22))),
                ("s4", Some(at("s4", "Dali", "China", 25.59, 100.19))),
                ("s5", Some(at("s5", "Sydney", "Australia", -33.86, 151.20))),
                ("s6", Some(at("s6", "Sydney", "AU", -33.87, 151.21))),
                ("s7", Some(at("s7", "Sydney", "Australia", -33.85, 151.19))),
                ("s8", Some(at("s8", "Sydney", "Australia", -33.88, 151.22))),
            ],
        )
        .expect("八家都返回");
        assert!(!merged.consistent, "4-4 平局必须判不一致");
        assert_eq!(merged.sources.len(), 8);
    }

    /// **只有 2 家返回且它们一致**（其余没答）⇒ 2 票就是全部 ⇒ 过半 ⇒ `true`。
    #[test]
    fn two_of_eight_agreeing_is_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("s1", Some(at("s1", "Dali", "China", 25.60, 100.20))),
                ("s2", Some(at("s2", "大理", "中国", 25.61, 100.21))),
                ("s3", None),
                ("s4", None),
                ("s5", None),
                ("s6", None),
                ("s7", None),
                ("s8", None),
            ],
        )
        .expect("两家返回");
        assert!(merged.consistent, "2/2 过半，且有人印证");
        assert_eq!(merged.sources.iter().filter(|s| s.contains("无结果")).count(), 6);
    }

    /// 八家全挂 ⇒ `None`（诚实路径不变）。
    #[test]
    fn all_eight_missing_yields_none() {
        let none: Option<GeoLocation> = None;
        let results: Vec<(&'static str, Option<GeoLocation>)> = ["s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8"]
            .into_iter()
            .map(|name| (name, none.clone()))
            .collect();
        assert!(merge_many("1.2.3.4", &results).is_none());
    }


    /// **两家互相矛盾**（其余没返回）⇒ 不许说一致；没返回的源也要如实列出。
    #[test]
    fn two_disagreeing_sources_are_not_consistent() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali", "China", 25.6, 100.2))),
                ("ip-api.com", Some(at("ip-api.com", "广州", "中国", 23.1, 113.2))),
                ("ipinfo.io", None),
                ("ifconfig.co", None),
            ],
        )
        .expect("两家返回");
        assert!(!merged.consistent, "没有第三家能印证 ⇒ 不是「多数一致」");
        assert_eq!(merged.sources.len(), 4, "没返回的源也要出现");
        assert_eq!(merged.sources.iter().filter(|s| s.contains("无结果")).count(), 2);
    }

    /// **只有一家返回** ⇒ 没人印证，`consistent = false`（四源版同样不许乐观：
    /// 「1 > 0」看似过半，但一家不可能构成「多数一致」）。
    #[test]
    fn single_source_is_not_consistent_and_lists_missing_ones() {
        let merged = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", Some(at("ipwho.is", "Dali", "China", 25.6, 100.2))),
                ("ip-api.com", None),
                ("ipinfo.io", None),
                ("ifconfig.co", None),
            ],
        )
        .expect("一家返回");
        assert!(!merged.consistent, "只有一家 ⇒ 没人印证");
        assert_eq!(merged.sources.len(), 4);
        assert_eq!(merged.sources.iter().filter(|s| s.contains("无结果")).count(), 3);
        assert_eq!(merged.source, "ipwho.is");
    }

    /// 四个源全挂 ⇒ `None`（界面必须报错，而不是画一个 (0,0) 的假点）。
    #[test]
    fn all_sources_missing_yields_none() {
        let none: Option<GeoLocation> = None;
        assert!(merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", none.clone()),
                ("ip-api.com", none.clone()),
                ("ipinfo.io", none.clone()),
                ("ifconfig.co", none),
            ]
        )
        .is_none());
    }

    /// **展示用的国家名绝不能是两字母代码**（`ipinfo.io` 给的就是 `HK`/`CN`）。
    ///
    /// 三种情形一起钉：混着来时中文全名胜出；只有代码时**宁可为空**（如实表示
    /// 「没有可展示的名称」）；端到端——只有 ipinfo 返回时最终 `country` 也不是 `"HK"`。
    #[test]
    fn final_country_is_never_a_two_letter_code() {
        assert_eq!(prefer_country_name(["HK", "中国"].iter().copied(), ""), "中国");
        assert_eq!(
            prefer_country_name(["HK", "Hong Kong"].iter().copied(), ""),
            "Hong Kong",
            "英文全名也胜过代码"
        );
        assert_eq!(
            prefer_country_name(["HK"].iter().copied(), "HK"),
            "",
            "只有代码时不许把代码当国家名"
        );

        // 端到端：ipinfo 是**唯一返回**的源（坐标基准就是它）⇒ country 也不能是 "HK"
        let only_ipinfo = merge_many(
            "1.2.3.4",
            &[
                ("ipwho.is", None),
                ("ip-api.com", None),
                ("ipinfo.io", Some(at("ipinfo.io", "Tung Chung", "HK", 22.2878, 113.9424))),
                ("ifconfig.co", None),
            ],
        )
        .expect("ipinfo 返回");
        assert_eq!(only_ipinfo.country, "", "没有全名时不许把 HK 当国家名");
        assert!(!is_country_code(&only_ipinfo.country));
        assert_eq!(only_ipinfo.city, "Tung Chung", "城市照常给");
    }

    // -----------------------------------------------------------------------
    // 0.9.1-C：命中率 —— 探测 TTL / 单飞 / serve-stale / 失败冷却 / /64 / LRU / 度量
    // -----------------------------------------------------------------------

    /// 测试替身：按脚本返回结果，并**记下**探测与坐标查询各发了几次。
    #[derive(Default)]
    struct FakeNet {
        probe_result: Option<String>,
        lookup_result: Option<GeoLocation>,
        probes: AtomicU32,
        lookups: AtomicU32,
    }

    impl FakeNet {
        fn answering(ip: &str, loc: GeoLocation) -> Self {
            Self {
                probe_result: Some(ip.to_string()),
                lookup_result: Some(loc),
                ..Default::default()
            }
        }
        fn probe_calls(&self) -> u32 {
            self.probes.load(Ordering::Relaxed)
        }
        fn lookup_calls(&self) -> u32 {
            self.lookups.load(Ordering::Relaxed)
        }
    }

    impl LocationNet for FakeNet {
        fn probe(&self) -> BoxFut<'_, Option<String>> {
            self.probes.fetch_add(1, Ordering::Relaxed);
            let r = self.probe_result.clone();
            Box::pin(async move { r })
        }
        fn lookup_self(&self, _probed: Option<String>) -> BoxFut<'_, Option<GeoLocation>> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            let r = self.lookup_result.clone();
            Box::pin(async move { r })
        }
        fn lookup_ip(&self, _ip: String) -> BoxFut<'_, Option<GeoLocation>> {
            self.lookups.fetch_add(1, Ordering::Relaxed);
            let r = self.lookup_result.clone();
            Box::pin(async move { r })
        }
    }

    fn cached_at(loc: &GeoLocation, fetched_unix: u64) -> CachedLocation {
        location_to_cached(loc, fetched_unix, Some("en0"))
    }

    /// 一个「该 IP 已有位置缓存」的缓存。
    fn cache_with(ip: &str, fetched_unix: u64) -> LocationCache {
        let mut cache = LocationCache::default();
        let (key, _) = key_or_raw(ip);
        cache.put(&key, cached_at(&at("ipwho.is", "Dali", "China", 25.6, 100.2), fetched_unix));
        cache
    }

    /// **L2**：短 TTL 内反复进页面 ⇒ 连探测都不发（`probe_cached=true`，零网络）。
    #[tokio::test]
    async fn probe_ttl_hit_skips_the_probe() {
        let now = 1_000_000u64;
        let mut cache = cache_with("1.2.3.4", now - 300);
        cache.last_probe = Some(LocationProbe { ip: "1.2.3.4".into(), checked_unix: now - 10 });
        // 这个替身任何网络调用都会让计数 > 0
        let net = FakeNet::default();

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls(), 0, "TTL 内不许发探测");
        assert_eq!(net.lookup_calls(), 0, "TTL 内不许发坐标查询");
        assert!(out.cache.probe_cached);
        assert!(out.cache.from_cache);
        assert!(!out.cache.stale, "这次是「刚验证过 IP」的正常命中，不是 stale");
        assert_eq!(out.cache.key_kind, "ip");
        assert_eq!(out.cache.age_s, Some(300));
        assert_eq!(cache.stats.hits, 1);
        assert_eq!(cache.stats.probe_cached, 1);
    }

    /// **L2 边界**：TTL 一过就重新探测（不能把「省请求」变成「永远不更新」）。
    #[tokio::test]
    async fn probe_ttl_expired_probes_again() {
        let now = 1_000_000u64;
        let mut cache = cache_with("1.2.3.4", now - 500);
        cache.last_probe = Some(LocationProbe {
            ip: "1.2.3.4".into(),
            checked_unix: now - LOCATION_PROBE_TTL_S - 1,
        });
        let net = FakeNet { probe_result: Some("1.2.3.4".into()), ..Default::default() };

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls(), 1, "过期后要重新探测");
        assert_eq!(net.lookup_calls(), 0, "IP 没变 ⇒ 仍不发坐标查询");
        assert!(out.cache.from_cache);
        assert!(!out.cache.probe_cached, "这次是真探测");
        assert_eq!(cache.last_probe.unwrap().checked_unix, now, "探测时间要刷新");
    }

    /// **L3**：两个 IP-only 源都不答 ⇒ 用 `last_probe.ip` 的条目 serve-stale，
    /// **不发坐标查询**，并把「无法确认」如实标成 `stale=true`。
    #[tokio::test]
    async fn probe_failure_serves_the_last_probe_entry_as_stale() {
        let now = 1_000_000u64;
        let mut cache = cache_with("1.2.3.4", now - 900);
        cache.last_probe = Some(LocationProbe {
            ip: "1.2.3.4".into(),
            checked_unix: now - LOCATION_PROBE_TTL_S - 5,
        });
        let net = FakeNet::default(); // probe ⇒ None

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls(), 1, "探测确实发了（它就是失败的那一步）");
        assert_eq!(net.lookup_calls(), 0, "serve-stale **不发**坐标查询");
        assert!(out.cache.stale, "无法确认 IP 是否变化 ⇒ stale");
        assert!(out.cache.from_cache);
        assert!(!out.cache.ip_changed, "没测到就不许说「变了」");
        assert_eq!(out.origin.expect("要给出上次可用结果").city, "Dali", "给的是上次那条缓存");
        assert_eq!(cache.stats.stale_hits, 1);
        assert_eq!(cache.stats.lookups, 0, "没有查过坐标");
    }

    /// **L4**：失败冷却内不再打任何网络；有上次可用结果就 stale 给出来。
    #[tokio::test]
    async fn failed_lookup_is_not_retried_within_the_cooldown() {
        let now = 1_000_000u64;
        let mut cache = cache_with("1.2.3.4", now - 50);
        cache.last_failure = Some(LocationFailure {
            ip: "1.2.3.4".into(),
            failed_unix: now - 10,
            reason: "四个坐标源都没返回".into(),
        });
        let net = FakeNet::default();

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls(), 0, "冷却内连探测都不发");
        assert_eq!(net.lookup_calls(), 0);
        assert!(out.cache.stale);
        assert!(out.cache.probe_cached);
        assert!(out.origin.is_some(), "有上次可用结果就不许空手而归");
    }

    /// 冷却内且**没有**可用缓存 ⇒ 如实报出真实原因（仍然一个请求都不发）。
    #[tokio::test]
    async fn cooldown_without_a_cached_entry_reports_the_real_reason() {
        let now = 1_000_000u64;
        // 用结构体更新而不是「先 Default 再赋字段」：后者会触发 clippy 的
        // `field_reassign_with_default`（本仓库 clippy 是 `-D warnings`）。
        let mut cache = LocationCache {
            last_failure: Some(LocationFailure {
                ip: String::new(),
                failed_unix: now - 5,
                reason: "四个坐标源都没返回".into(),
            }),
            ..Default::default()
        };
        let net = FakeNet::default();

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls() + net.lookup_calls(), 0, "冷却内不打网络");
        assert!(out.origin.is_none());
        let err = out.error.expect("要如实报错");
        assert!(err.contains("四个坐标源都没返回"), "真实原因要带上：{err}");
        assert!(out.cache.stale);
    }

    /// **L4 边界**：冷却一过就恢复（否则「省请求」变成「永远不重试」）。
    #[tokio::test]
    async fn cooldown_expired_retries() {
        let now = 1_000_000u64;
        let mut cache = cache_with("1.2.3.4", now - 5_000);
        cache.last_probe = Some(LocationProbe {
            ip: "1.2.3.4".into(),
            checked_unix: now - LOCATION_PROBE_TTL_S - 1,
        });
        cache.last_failure = Some(LocationFailure {
            ip: "1.2.3.4".into(),
            failed_unix: now - LOCATION_FAIL_COOLDOWN_S - 1,
            reason: "上一次失败".into(),
        });
        let net = FakeNet::default(); // 探测失败 ⇒ 走 serve-stale

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.probe_calls(), 1, "冷却过期后必须重新探测");
        assert!(out.origin.is_some());
        assert!(out.cache.stale);
    }

    /// **L1**：并发的两次调用（预热 + 用户点开）只跑一次查询 ——
    /// 第二个在闸门后重新读缓存，TTL 记忆已写好 ⇒ 一个请求都不发。
    #[tokio::test]
    async fn concurrent_calls_run_one_lookup() {
        let cache = std::sync::Arc::new(Mutex::new(LocationCache::default()));
        let lookups = std::sync::Arc::new(AtomicU32::new(0));
        let one = |cache: std::sync::Arc<Mutex<LocationCache>>,
                   lookups: std::sync::Arc<AtomicU32>| async move {
            single_flight(|| async move {
                let mut c = cache.lock().await;
                let key = key_or_raw("1.2.3.4").0;
                if c.has(&key) {
                    return; // 第二个调用在这里命中（这就是闸门的意义）
                }
                lookups.fetch_add(1, Ordering::Relaxed);
                c.put(&key, cached_at(&at("ipwho.is", "Dali", "China", 25.6, 100.2), 1));
            })
            .await
        };

        tokio::join!(one(cache.clone(), lookups.clone()), one(cache.clone(), lookups.clone()));

        assert_eq!(lookups.load(Ordering::Relaxed), 1, "并发两次只能查一次");
    }

    /// 结构守卫：**两个入口都必须经过单飞闸门** —— 否则「预热 + 用户点开」
    /// 会各跑一次四源查询（那正是 L1 要消掉的重复）。
    #[test]
    fn both_entry_points_use_the_single_flight_gate() {
        let src = include_str!("globe.rs");
        let body = &src[..src.find("#[cfg(test)]").expect("测试模块")];
        let gates = body.matches("single_flight(").count();
        assert_eq!(gates, 2, "globe_data 与 prewarm 各一次，实际 {gates}");
    }

    /// **L5**：IPv6 按 /64 聚合 —— 接口标识轮换后位置没变，仍命中同一条缓存。
    #[tokio::test]
    async fn ipv6_is_keyed_by_64_bit_prefix() {
        let now = 1_000_000u64;
        let first = "2001:db8:1:2:aaaa:aaaa:aaaa:aaaa";
        let rotated = "2001:db8:1:2:bbbb:bbbb:bbbb:bbbb";
        let mut cache = cache_with(first, now - 100);
        cache.last_probe = Some(LocationProbe { ip: first.into(), checked_unix: now - 10 });
        let net = FakeNet { probe_result: Some(rotated.into()), ..Default::default() };

        let out = run_call(&net, &mut cache, now, false, None, Some("en0")).await;

        assert_eq!(net.lookup_calls(), 0, "同 /64 前缀 ⇒ 命中缓存，不查坐标");
        assert!(out.cache.from_cache);
        assert_eq!(out.cache.key_kind, "ipv6-prefix", "界面要能读出粒度是前缀");
        assert_eq!(out.origin.expect("应当命中").ip, "2001:db8:1:2::/64");
    }

    /// **L5**：IPv4-mapped IPv6 归一成 v4（同一个地址不该有两种键）。
    #[test]
    fn ipv4_mapped_ipv6_normalizes_to_v4() {
        assert_eq!(
            key_or_raw("::ffff:1.2.3.4"),
            ("1.2.3.4".to_string(), LocationKeyKind::Ip)
        );
        assert_eq!(key_or_raw("1.2.3.4").0, "1.2.3.4");
        assert_eq!(key_or_raw("not-an-ip").0, "not-an-ip", "解析不了就退回原串");
    }

    /// **L6**：超过上限按最旧淘汰；元数据（last_probe/stats）不受影响。
    #[test]
    fn cache_evicts_the_oldest_beyond_the_limit() {
        // 元数据也用结构体更新初始化，避免 `Default::default()` 之后再赋字段（clippy）。
        let mut cache = LocationCache {
            last_probe: Some(LocationProbe { ip: "1.2.3.4".into(), checked_unix: 1 }),
            ..Default::default()
        };
        for i in 0..(LOCATION_CACHE_MAX_ENTRIES as u64 + 5) {
            let (key, _) = key_or_raw(&format!("10.0.0.{i}"));
            cache.put(&key, cached_at(&at("ipwho.is", "Dali", "China", 25.6, 100.2), i));
        }
        assert_eq!(cache.entries.len(), LOCATION_CACHE_MAX_ENTRIES + 5);

        let evicted = cache.evict_oldest_beyond(LOCATION_CACHE_MAX_ENTRIES);

        assert_eq!(evicted, 5, "多出来的 5 条要淘汰");
        assert_eq!(cache.entries.len(), LOCATION_CACHE_MAX_ENTRIES);
        assert!(!cache.has(&key_or_raw("10.0.0.0").0), "最旧的先走");
        assert!(cache.has(&key_or_raw(&format!("10.0.0.{}", LOCATION_CACHE_MAX_ENTRIES + 4)).0));
        assert!(cache.last_probe.is_some(), "元数据不计入上限");
    }

    /// **L8**：`calls / hits / lookups / probe_calls / probe_cached` 分开记 ——
    /// 没有这组数，命中率只能靠嘴说。
    #[tokio::test]
    async fn stats_counts_calls_hits_and_lookups() {
        let now = 1_000_000u64;
        let mut cache = LocationCache::default();
        let net = FakeNet::answering("1.2.3.4", at("ipwho.is", "Dali", "China", 25.6, 100.2));

        let first = run_call(&net, &mut cache, now, false, None, Some("en0")).await;
        assert_eq!(first.probe_sent, 1, "首查要探测");
        assert_eq!(first.lookups_sent, 1, "首查要查坐标");

        let second = run_call(&net, &mut cache, now + 5, false, None, Some("en0")).await;
        assert_eq!(second.probe_sent, 0);
        assert_eq!(second.lookups_sent, 0);

        assert_eq!(cache.stats.calls, 2);
        assert_eq!(cache.stats.hits, 1);
        assert_eq!(cache.stats.lookups, 1, "第二次没有再查坐标");
        assert_eq!(cache.stats.probe_calls, 1, "第二次没有真探测");
        assert_eq!(cache.stats.probe_cached, 1);
        assert_eq!(cache.stats.stale_hits, 0);
    }

    /// **序列级主证据**：连开 5 次页面 ⇒ **探测 1 次、坐标查询 1 次**。
    #[tokio::test]
    async fn repeated_page_opens_do_one_lookup() {
        let t0 = 1_000_000u64;
        let mut cache = LocationCache::default();
        let net = FakeNet::answering("1.2.3.4", at("ipwho.is", "Dali", "China", 25.6, 100.2));

        for i in 0..5u64 {
            let out = run_call(&net, &mut cache, t0 + i, false, None, Some("en0")).await;
            assert!(out.origin.is_some(), "每次都应当有位置可显示");
        }

        assert_eq!(net.probe_calls(), 1, "5 次打开只探测一次（改前是 5 次）");
        assert_eq!(net.lookup_calls(), 1, "5 次打开只查一次坐标（改前最多 5 次）");
        assert_eq!(cache.stats.calls, 5);
        assert_eq!(cache.stats.hits, 4);
        assert_eq!(cache.stats.lookups, 1);
        assert_eq!(cache.stats.probe_cached, 4);
    }

    /// IP-only 探测（**四个**源）：任一成功即可；按加入顺序取优先级；垃圾不算「查到了 IP」。
    #[test]
    fn ip_only_probes_parse_and_reject_garbage() {
        // 逐个源单独可用
        assert_eq!(
            pick_public_ip(Some("  116.53.173.241\n"), None, None, None).as_deref(),
            Some("116.53.173.241")
        );
        let trace = "fl=abc\nh=1.1.1.1\nip=2001:db8::1\nts=1700000000\n";
        assert_eq!(
            pick_public_ip(None, Some(trace), None, None).as_deref(),
            Some("2001:db8::1")
        );
        assert_eq!(
            pick_public_ip(None, None, Some(r#"{"ipString":"1.2.3.4","ipType":"IPv4"}"#), None)
                .as_deref(),
            Some("1.2.3.4")
        );
        assert_eq!(
            pick_public_ip(None, None, None, Some(r#"{"ip":"5.6.7.8","country":"HK"}"#)).as_deref(),
            Some("5.6.7.8")
        );

        // 优先级：ipify → cdn-cgi/trace → bigdatacloud → country.is（结果确定）
        assert_eq!(
            pick_public_ip(
                Some("1.2.3.4"),
                Some("ip=5.6.7.8"),
                Some(r#"{"ipString":"9.9.9.9"}"#),
                Some(r#"{"ip":"8.8.8.8"}"#)
            )
            .as_deref(),
            Some("1.2.3.4"),
            "四个都答 ⇒ 取最先加入的那个"
        );
        assert_eq!(
            pick_public_ip(None, Some("ip=5.6.7.8"), Some(r#"{"ipString":"9.9.9.9"}"#), None)
                .as_deref(),
            Some("5.6.7.8")
        );
        assert_eq!(
            pick_public_ip(None, None, Some("<html>blocked</html>"), Some(r#"{"ip":"8.8.8.8"}"#))
                .as_deref(),
            Some("8.8.8.8"),
            "前面那个坏了就往后走"
        );

        // 全挂 / 垃圾：都不许当成一个公网 IP
        assert_eq!(pick_public_ip(Some("rate limited"), Some("nope"), None, None), None);
        assert_eq!(pick_public_ip(Some("1.2.3.4:80"), None, None, None), None, "端口不能混进 IP");
        assert_eq!(pick_public_ip(None, None, Some(r#"{"ipString":""}"#), Some(r#"{"ip":"x"}"#)), None);
        assert_eq!(pick_public_ip(None, None, None, None), None);
    }

    /// `ipinfo.io`：`loc` 是 `"lat,lon"` **字符串**、`country` 是**两字母代码**、
    /// ISP 在 `org`（含 AS 号，原样保留）；缺 `loc`/非数字/越界都不算查到。
    #[test]
    fn ipinfo_parses_loc_and_keeps_the_code_out_of_display() {
        let body = r#"{"ip":"45.207.197.185","city":"Tung Chung","region":"Islands",
                       "country":"HK","loc":"22.2878,113.9424",
                       "org":"AS401701 cognetcloud INC"}"#;
        let l = parse_ipinfo(body).expect("应当解析成功");
        assert_eq!(l.source, "ipinfo.io");
        assert_eq!(l.city, "Tung Chung");
        assert_eq!(l.country, "HK");
        assert_eq!(l.isp, "AS401701 cognetcloud INC", "org 原样保留（AS 号对排障有用）");
        assert!((l.lat - 22.2878).abs() < 1e-9);
        assert!((l.lon - 113.9424).abs() < 1e-9);
        assert!(is_country_code(&l.country), "这一家的 country 就是代码 ⇒ 展示层必须挡");

        assert!(parse_ipinfo(r#"{"city":"x"}"#).is_none(), "缺 ip/loc 不算查到");
        assert!(parse_ipinfo(r#"{"ip":"1.2.3.4","loc":"not-numbers"}"#).is_none());
        assert!(parse_ipinfo(r#"{"ip":"1.2.3.4","loc":"95.0,10.0"}"#).is_none(), "纬度越界");
        assert!(parse_ipinfo("not json").is_none());
    }

    /// `ifconfig.co`：数字坐标 + **全名**国家；**`?ip=` 那条路径没有坐标字段**，
    /// 必须 `None` —— 绝不许当成 (0,0)（否则会以「几内亚湾」参与四源坐标投票）。
    #[test]
    fn ifconfig_parses_numbers_and_rejects_missing_coordinates() {
        let body = r#"{"ip":"45.207.197.185","country":"Hong Kong","country_iso":"HK",
                       "city":"Hong Kong","latitude":22.2842,"longitude":114.1759,
                       "asn_org":"High Family Technology Co., Limited"}"#;
        let l = parse_ifconfig_co(body).expect("应当解析成功");
        assert_eq!(l.source, "ifconfig.co");
        assert_eq!(l.country, "Hong Kong", "全名（不用 country_iso）");
        assert_eq!(l.isp, "High Family Technology Co., Limited");
        assert!((l.lat - 22.2842).abs() < 1e-9);

        // 本机实测：`/json?ip=1.1.1.1` 返回国家与 ASN，但**没有** latitude/longitude
        let no_coords = r#"{"ip":"1.1.1.1","country":"Australia","country_iso":"AU",
                            "asn_org":"CLOUDFLARENET"}"#;
        assert!(
            parse_ifconfig_co(no_coords).is_none(),
            "缺坐标 ⇒ None（关键：不许 default 成 (0,0) 去投票）"
        );
        assert!(parse_ifconfig_co(r#"{"ip":"1.2.3.4","latitude":null,"longitude":null}"#).is_none());
        assert!(parse_ifconfig_co(r#"{"ip":"1.2.3.4","latitude":0.0,"longitude":0.0}"#).is_some(),
            "(0,0) **显式给出**是合法坐标，不该被误拒");
        assert!(parse_ifconfig_co(r#"{"latitude":1.0,"longitude":2.0}"#).is_none(), "缺 ip");
        assert!(parse_ifconfig_co("not json").is_none());
    }

    // -----------------------------------------------------------------------
    // 0.9.1-E：四个新源（**先实测可达再写进来**；形状来自本机 curl）
    // -----------------------------------------------------------------------

    /// `ipwhois.app`：`success` 必须为 true；**缺坐标 → None**（不许 default 成 0）。
    #[test]
    fn ipwhois_app_parses_success_and_rejects_missing_coordinates() {
        let body = r#"{"ip":"45.207.197.185","success":true,"type":"IPv4",
                       "country":"Hong Kong","city":"Hong Kong",
                       "latitude":22.2783168,"longitude":114.1746891,
                       "connection":{"isp":"Vapeline Technology","org":"X"}}"#;
        let l = parse_ipwhois_app(body).expect("应当解析成功");
        assert_eq!(l.source, "ipwhois.app");
        assert_eq!(l.city, "Hong Kong");
        assert_eq!(l.isp, "Vapeline Technology");
        assert!((l.lat - 22.2783168).abs() < 1e-9);

        assert!(parse_ipwhois_app(r#"{"ip":"1.2.3.4","success":false}"#).is_none());
        assert!(
            parse_ipwhois_app(r#"{"ip":"1.2.3.4","success":true,"city":"X"}"#).is_none(),
            "缺坐标不许被 serde default 成 (0,0)"
        );
        assert!(parse_ipwhois_app("<html>challenge</html>").is_none());
    }

    /// `json.geoiplookup.io`：用 `country_name`（全名），不用 `country_code`。
    #[test]
    fn geoiplookup_parses_success_and_rejects_missing_coordinates() {
        let body = r#"{"ip":"45.207.197.185","isp":"Vapeline Technology","org":"",
                       "latitude":22.3193,"longitude":114.169,"city":"Hong Kong",
                       "country_code":"HK","country_name":"Hong Kong"}"#;
        let l = parse_geoiplookup(body).expect("应当解析成功");
        assert_eq!(l.source, "geoiplookup.io");
        assert_eq!(l.country, "Hong Kong");
        assert_eq!(l.isp, "Vapeline Technology");
        assert!((l.lon - 114.169).abs() < 1e-9);

        assert!(
            parse_geoiplookup(r#"{"ip":"1.2.3.4","city":"X","country_name":"Y"}"#).is_none(),
            "缺坐标 ⇒ None"
        );
        assert!(parse_geoiplookup(r#"{"isp":"x"}"#).is_none(), "缺 ip ⇒ None");
        assert!(parse_geoiplookup("not json").is_none());
    }

    /// `freeipapi.com`（**307 重定向**）：`ipAddress`/`latitude`/`longitude`/`countryName`/`cityName`。
    #[test]
    fn freeipapi_parses_success_and_rejects_missing_coordinates() {
        let body = r#"{"ipVersion":4,"ipAddress":"45.207.197.185","latitude":22.3193,
                       "longitude":114.169,"countryName":"Hong Kong","countryCode":"HK",
                       "cityName":"Hong Kong","regionName":"Kowloon"}"#;
        let l = parse_freeipapi(body).expect("应当解析成功");
        assert_eq!(l.source, "freeipapi.com");
        assert_eq!(l.city, "Hong Kong");
        assert_eq!(l.country, "Hong Kong");
        assert!((l.lat - 22.3193).abs() < 1e-9);

        assert!(
            parse_freeipapi(r#"{"ipAddress":"1.2.3.4","countryName":"X"}"#).is_none(),
            "缺坐标 ⇒ None"
        );
        assert!(parse_freeipapi(r#"{"latitude":1.0,"longitude":2.0}"#).is_none(), "缺 ip ⇒ None");
        assert!(parse_freeipapi("<html>redirect target</html>").is_none());
    }

    /// `api.ipquery.io`：坐标在**嵌套的 `location`** 里；顶层没有坐标字段。
    #[test]
    fn ipquery_parses_nested_location_and_rejects_missing_coordinates() {
        let body = r#"{"ip":"45.207.197.185",
                       "isp":{"asn":"AS137899","org":"I LAYER LIMITED","isp":"I LAYER LIMITED"},
                       "location":{"country":"Hong Kong","country_code":"HK","city":"Hong Kong",
                                   "state":"Kowloon","latitude":22.300523882689273,
                                   "longitude":114.17668557541151}}"#;
        let l = parse_ipquery(body).expect("应当解析成功");
        assert_eq!(l.source, "ipquery.io");
        assert_eq!(l.city, "Hong Kong");
        assert_eq!(l.isp, "I LAYER LIMITED");
        assert!((l.lat - 22.300523882689273).abs() < 1e-9);

        assert!(
            parse_ipquery(r#"{"ip":"1.2.3.4","location":{"city":"X"}}"#).is_none(),
            "location 里缺坐标 ⇒ None"
        );
        assert!(parse_ipquery(r#"{"ip":"1.2.3.4"}"#).is_none(), "缺 location ⇒ None");
        assert!(parse_ipquery(r#"{"location":{"latitude":1.0,"longitude":2.0}}"#).is_none(), "缺 ip");
        assert!(parse_ipquery("not json").is_none());
    }

    /// 缓存条目 ↔ 对外位置：字段一个不丢；**抓取时绑没绑卡必须留在条目里**
    /// （A21 靠它决定缓存命中时敢不敢说「本机」）。
    #[test]
    fn cache_entry_roundtrips_through_the_globe_helpers() {
        let original = at("ipinfo.io", "大理", "中国", 25.6, 100.2);
        let cached_entry = location_to_cached(&original, 1_790_000_000, Some("en0"));
        assert_eq!(cached_entry.fetched_unix, 1_790_000_000);
        assert_eq!(cached_entry.bound_interface.as_deref(), Some("en0"));

        let back = cached_to_location("116.53.173.241", &cached_entry);
        assert_eq!(back.ip, "116.53.173.241", "IP 是键，不进条目");
        assert_eq!(back.city, "大理");
        assert_eq!(back.country, "中国");
        assert_eq!(back.lat, original.lat);
        assert_eq!(back.lon, original.lon);
        assert_eq!(back.isp, original.isp);
        assert_eq!(back.source, "ipinfo.io");
        assert_eq!(back.sources, original.sources);

        assert_eq!(
            location_to_cached(&original, 1, None).bound_interface,
            None,
            "没绑卡那次抓取要如实留下 None"
        );
    }

    /// **A21 在缓存命中时也不许松口**：缓存那次没绑卡 ⇒ 命中时仍 `trusted = false`，
    /// 且原因要说得对（是「缓存时没绑卡」，不是 `judge` 里那句「读不到物理默认路由」）。
    #[test]
    fn cached_location_never_upgrades_an_unbound_fetch_to_trusted() {
        let origin = cached_to_location("1.2.3.4", &entry(Some("en0")));

        let bound = self_check_from_cache(&entry(Some("en0")), &origin);
        assert!(bound.trusted, "当时绑了网卡且拿到了位置 ⇒ 与在线查询同口径");
        assert_eq!(bound.bound_interface.as_deref(), Some("en0"));
        assert!(bound.reason.is_none());

        let unbound = self_check_from_cache(&entry(None), &origin);
        assert!(!unbound.trusted, "当时没绑卡 ⇒ 缓存命中也不许说「本机」");
        assert!(
            unbound.reason.as_deref().is_some_and(|r| r.contains("缓存")),
            "原因要针对「缓存」这个场景：{:?}",
            unbound.reason
        );
    }

    /// 接口冻结：字段名逐字对齐 UI 的 `types.ts`（改名字前端就读不到，且**不报错**）。
    #[test]
    fn globe_cache_info_uses_the_frozen_field_names() {
        let json = serde_json::to_string(&GlobeCacheInfo {
            from_cache: true,
            fetched_unix: Some(7),
            ip_changed: false,
            stale: false,
            probe_cached: true,
            key_kind: "ipv6-prefix".into(),
            age_s: Some(3),
        })
        .unwrap();
        for needle in [
            "\"from_cache\":true",
            "\"fetched_unix\":7",
            "\"ip_changed\":false",
            "\"stale\":false",
            "\"probe_cached\":true",
            "\"key_kind\":\"ipv6-prefix\"",
            "\"age_s\":3",
        ] {
            assert!(json.contains(needle), "缺少冻结字段 {needle}：{json}");
        }

        let data = GlobeData {
            route: None,
            origin: None,
            error: Some("x".into()),
            self_check: SelfCheck::judge(None, None),
            cache: GlobeCacheInfo {
                from_cache: false,
                fetched_unix: None,
                ip_changed: true,
                stale: true,
                probe_cached: false,
                key_kind: "ip".into(),
                age_s: None,
            },
        };
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains("\"cache\":{"), "`GlobeData` 必须带 cache 字段：{json}");
    }
}
