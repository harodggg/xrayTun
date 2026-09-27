//! TUN 会话编排：**建立 → 增量落盘 → 失败即回滚**。
//!
//! 这个模块是 `xt-tun` 的门面，也是整个特权 helper 唯一真正干活的地方。
//! 它的正确性标准只有一条：**任何一步失败，系统都必须回到动手之前的样子。**
//!
//! 为此做了三件事：
//!
//! 1. **先记账再动手**。会话快照在创建 utun 之后立刻落盘，之后每加一条路由、
//!    每改一个服务的 DNS 都再落一次盘。这样即使进程在任意时刻被杀，
//!    下次启动也能读到「已经做了什么」并精确回滚。
//! 2. **回滚顺序与安装顺序相反**，且每一项失败都不中断其余项
//!    （回滚路径上「尽力而为」比「严格失败」更重要）。
//! 3. **DNS 先于路由还原**。反过来的话，会有一段「流量已出隧道、但 DNS
//!    还指向隧道内哨兵地址」的窗口，用户会看到几秒钟的全网解析失败。
//! 4. **回滚之后要复检**（不是「恢复命令发出去了」就算完）：默认路由必须仍可用、
//!    本会话的捕获路由必须已从 utun 上删掉、DNS 必须**逐值**回到备份
//!    （含「原本没有设置任何 DNS 服务器」这一态）。任何一条不满足都如实上报
//!    并保留快照 —— **绝不假装成功**（见 `rollback`）。

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::io::RawFd;

use xt_proto::{Cidr, DatapathPlan, DnsMode, InstalledRoute, RouteVia, TunUpRequest};

use crate::error::{Error, Result};
use crate::macos::dns;
use crate::macos::netif;
use crate::macos::route;
use crate::macos::snapshot::{SessionSnapshot, SessionState};
use crate::macos::utun::UtunDevice;
use crate::plan::{
    build_plan, rollback_plan, PhysicalUplink, PlannedRoute, RollbackAction, RouteKind, TunPlan,
};

pub struct BringUpOutcome {
    pub snapshot: SessionSnapshot,
    pub plan: TunPlan,
    /// `HandoffFd` 模式下要交给调用方的 utun fd；`HelperSpawn` 模式下为 `None`。
    pub handed_fd: Option<RawFd>,
}

/// 建一份新会话快照，并把**上一个会话的信任锚记录带过来**。
///
/// # 为什么必须带（P0-2）
///
/// `bring_up` 紧接着会 `save()`，那是**整份覆盖**磁盘上的旧快照。旧快照里若记着
/// 信任锚，而它对应的根证书已经装进 `System.keychain`，记录一丢就再也找不回来：
/// GUI 只按**本会话** CA 的指纹删，helper 的 `RemoveTrustAnchor` 又要求活跃会话。
/// 结果是用户**再也删不掉**那张证书。
///
/// 带过来之后，这份记录会跟着新会话走：`TunDown`/`rollback`/`force_cleanup`
/// 都会遍历 `trust_anchors` 并调用 `trust::rollback`（安装前已存在的锚按记录跳过，
/// 我们装的那些会被删掉），于是旧指纹**始终可被列出、可被删除**。
///
/// 抽成独立函数是为了能在**没有 root、没有 utun** 的测试里钉住这条不变量
/// （`bring_up` 本身要先建 utun，没法在普通用户下跑）。
fn new_snapshot_adopting_leftover_anchors(
    session_id: String,
    interface: String,
    physical: PhysicalUplink,
) -> Result<SessionSnapshot> {
    let mut snap = SessionSnapshot::new(session_id, interface, physical);
    match SessionSnapshot::load() {
        Ok(Some(prev)) if prev.has_trust_anchors() => {
            tracing::warn!(
                old_session = %prev.session_id,
                anchors = prev.trust_anchors.len(),
                "上一个会话留下了信任锚记录；已并入新会话（否则这些证书会永久留在钥匙串里）"
            );
            // 顺序刻意不翻转：`rollback` 自己按 `.rev()` 撤销。
            snap.trust_anchors = prev.trust_anchors;
        }
        Ok(_) => {}
        Err(e) => {
            // 读不出来不该让建立失败 —— 但必须可见：读不出就带不走记录，
            // 那正是"证书会永久残留"的入口。
            tracing::error!(error = %e, "读取旧快照失败，无法把遗留信任锚并入新会话");
        }
    }
    Ok(snap)
}

/// 建立 TUN 会话。
pub fn bring_up(req: &TunUpRequest) -> Result<BringUpOutcome> {
    // ---- 0) 前置校验：把非法输入挡在动系统之前 ----
    if req.addresses.is_empty() {
        return Err(Error::Invalid("TUN 地址列表为空".into()));
    }
    if req.mtu < 576 {
        return Err(Error::Invalid(format!("MTU {} 过小（下限 576）", req.mtu)));
    }

    // ---- 1) 探测物理出口 ----
    let dr = route::default_route()?;
    let physical = PhysicalUplink {
        service: dns::service_for_device(&dr.interface).ok(),
        interface: dr.interface,
        gateway: dr.gateway,
    };
    tracing::info!(
        interface = %physical.interface,
        gateway = ?physical.gateway,
        service = ?physical.service,
        "探测到物理出口"
    );

    let plan = build_plan(req, physical.clone())?;

    // ---- 2) 建 utun ----
    let unit = req.interface_name.as_deref().and_then(parse_utun_unit);
    let device = UtunDevice::create(unit)?;
    let ifname = device.name().to_string();
    tracing::info!(interface = %ifname, "utun 已创建");

    // 立刻落一笔「我开始动手了」，这是崩溃可恢复的关键。
    //
    // ⚠️ **不能直接 `SessionSnapshot::new`**：这一步的 `save()` 会整份覆盖磁盘上的
    // 旧快照。旧快照里若记着信任锚（本地根证书已经装进 `System.keychain`），
    // 记录一丢，那张证书就再也没有任何路径能删掉（见 P0-2 与
    // `new_snapshot_adopting_leftover_anchors`）。
    let mut snap =
        new_snapshot_adopting_leftover_anchors(req.session_id.clone(), ifname.clone(), physical)?;
    snap.save()?;

    // ---- 3) 逐步施加，任何一步失败都整体回滚 ----
    if let Err(e) = apply(&mut snap, &plan, &ifname, req.defer_default_routes) {
        tracing::error!(error = %e, session = %snap.session_id, "建立失败，开始回滚");
        if let Err(re) = rollback(&snap) {
            // 回滚也失败是最坏情况：必须留下足够明显的日志，并且**保留快照**，
            // 让下次启动的 restore 再试一次。
            tracing::error!(error = %re, "回滚失败，快照已保留，下次启动会重试");
            return Err(Error::Invalid(format!("{e}；且回滚失败: {re}")));
        }
        return Err(e);
    }

    // 延迟模式下会话还没真正生效，状态停在 BringingUp，
    // 这样 is_stale() 为真，中途崩溃能被下次启动的 restore 兜住。
    if !req.defer_default_routes {
        snap.state = SessionState::Up;
        snap.save()?;
    }

    let handed_fd = match &req.datapath {
        // 推荐路径：fd 交给调用方，数据面以普通用户身份运行。
        DatapathPlan::HandoffFd => Some(device.into_raw_fd()),
        // 兼容路径：helper 自己持有 fd 并拉起 root 数据面。
        // 注意 fd 必须留着 —— 一旦关闭，utun 接口就会消失。
        DatapathPlan::SpawnDatapath { .. } => {
            let fd = device.as_raw_fd();
            std::mem::forget(device);
            Some(fd)
        }
    };

    Ok(BringUpOutcome { snapshot: snap, plan, handed_fd })
}

fn apply(
    snap: &mut SessionSnapshot,
    plan: &TunPlan,
    ifname: &str,
    defer_default_routes: bool,
) -> Result<()> {
    // ---- 阶段 1a：接口地址 ----
    for addr in &plan.addresses {
        netif::configure_address(ifname, addr, plan.mtu)?;
    }

    // ---- 阶段 1b：bypass 路由 + 物理网卡作用域默认路由 ----
    // 这些路由都指向物理出口，装上不会造成任何黑洞，所以立刻装。
    for planned in &plan.routes {
        if !matches!(
            planned.kind,
            RouteKind::Bypass | RouteKind::PhysicalScopedDefault
        ) {
            continue;
        }
        install_route_or_skip(snap, planned, &resolve_via(&planned.via, ifname))?;
    }

    // ---- 阶段 1c：默认接管路由 ----
    // 延迟模式下只记账不装；否则立刻装（单阶段模式）。
    for planned in &plan.routes {
        if planned.kind != RouteKind::DefaultCapture {
            continue;
        }
        let via = resolve_via(&planned.via, ifname);
        if defer_default_routes {
            // 记进 pending：崩溃时回滚逻辑会尝试删除它（删不存在的路由是安全的空操作），
            // 因此「不确定装没装」也能被正确处理。
            //
            // `replaced` 此刻还是 `None`：**它要到真正安装的那一刻**（`commit_routes_and_dns`）
            // 才去查「这个前缀上原本有什么」—— 提前查会查到还没被顶掉的自己。
            snap.pending_routes.push(InstalledRoute {
                destination: planned.destination,
                via,
                replaced: None,
            });
            snap.save()?;
        } else {
            install_route_or_skip(snap, planned, &via)?;
        }
    }

    // ---- 阶段 1d：DNS ----
    // 非延迟模式才在这里改 DNS。延迟模式下 DNS 与默认路由一起在 commit 阶段生效，
    // 否则「DNS 已指向隧道内哨兵地址、但数据面还没起来」同样会造成解析全挂。
    if !defer_default_routes {
        apply_dns(snap, plan)?;
    }

    Ok(())
}

/// 安装一条路由。失败时按 `critical` 决定是放弃还是跳过。
///
/// 「跳过」不是吞掉错误：会打一条 `warn`，并且**不记进快照**
/// （因为确实没装上，回滚时也不该去删它）。
fn install_route_or_skip(
    snap: &mut SessionSnapshot,
    planned: &PlannedRoute,
    via: &RouteVia,
) -> Result<()> {
    // **装之前先记下同前缀上原本是什么**（task-85）：`route add` 是同前缀替换，
    // 不记就永远不知道顶掉了什么，回滚只能删不能恢复 —— 那正是 127/8 空洞的成因。
    let replaced = route::existing_route(&planned.destination).and_then(|r| r.to_via());
    match route::add(&planned.destination, via) {
        Ok(()) => {
            snap.installed_routes.push(InstalledRoute {
                destination: planned.destination,
                via: via.clone(),
                replaced,
            });
            // 增量落盘：即使下一条路由就崩了，这条也能被回滚。
            snap.save()?;
            Ok(())
        }
        Err(e) if !planned.critical => {
            tracing::warn!(
                destination = %planned.destination,
                error = %e,
                "非关键 bypass 路由安装失败，已跳过（不影响隧道建立）"
            );
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn apply_dns(snap: &mut SessionSnapshot, plan: &TunPlan) -> Result<()> {
    if plan.dns_servers.is_empty() {
        return Ok(());
    }
    for service in dns_services(plan, snap) {
        let backup = dns::backup(&service)?;
        snap.dns_backups.push(backup);
        snap.save()?;
        dns::set_dns(&service, &plan.dns_servers)?;
    }
    Ok(())
}

/// 两阶段启动的第二步：**现在**接管默认路由并切换 DNS。
///
/// 调用前提是数据面已经起来并确认可用（SOCKS 端口可连）。
/// 否则这不是「优化」，而是把黑洞窗口挪到了这里。
///
/// 之所以要同时传 `plan`：DNS 服务器列表来自 `TunPlan`，而 `TunPlan` 不落盘
/// （它含非序列化的中间状态），所以只能由在内存里持有它的调用方传进来。
pub fn commit_routes_and_dns(snap: &mut SessionSnapshot, plan: &TunPlan) -> Result<()> {
    for mut installed in std::mem::take(&mut snap.pending_routes) {
        // **接管默认路由之前也要先记账**（task-85）：同前缀的 `route add` 是替换，
        // 这里顶掉的可能是**另一个 VPN 的 `0.0.0.0/1`+`128.0.0.0/1`**（用户机器上
        // 就有 Karing / Tailscale）或内核的接口路由 —— 不记下来，回滚就只删不恢复。
        installed.replaced = route::existing_route(&installed.destination).and_then(|r| r.to_via());
        route::add(&installed.destination, &installed.via)?;
        snap.installed_routes.push(installed);
        snap.save()?;
    }
    apply_dns(snap, plan)?;
    snap.state = SessionState::Up;
    snap.save()?;
    Ok(())
}

/// 决定要对哪些网络服务改 DNS。
fn dns_services(plan: &TunPlan, snap: &SessionSnapshot) -> Vec<String> {
    match &plan.dns_mode {
        DnsMode::LeaveAlone => Vec::new(),
        DnsMode::Automatic => snap.physical.service.clone().into_iter().collect(),
        DnsMode::Explicit { services } => services.clone(),
    }
}

/// 把计划里的占位接口名（空串）替换成真实的 utun 名。
fn resolve_via(via: &RouteVia, ifname: &str) -> RouteVia {
    match via {
        RouteVia::Interface { name } if name.is_empty() => {
            RouteVia::Interface { name: ifname.to_string() }
        }
        RouteVia::ScopedInterface { name, gateway } if name.is_empty() => {
            RouteVia::ScopedInterface { name: ifname.to_string(), gateway: *gateway }
        }
        other => other.clone(),
    }
}

/// 逐项执行回滚动作，**尽力而为**：单项失败只记一条文案，不中断其余项。
///
/// 这个 helper 刻意**只做三件事**，其余全留给调用方，以保证抽出它不改变行为：
///
/// * **迭代顺序 = `items` 的顺序，绝不重排。** 所以收的是 `Iterator` 而不是
///   `DoubleEndedIterator` —— 后者会暗示本函数自己会反向遍历，而它不会；
///   方向（信任锚/DNS 的 `.rev()`、路由用 `rollback_plan` 算好的顺序）由三处调用点
///   各自决定，抽函数前后逐字不变。
/// * **失败文案由 `f` 逐字给出**，helper 只负责收集：等价于原来的
///   `if let Err(e) { failures.push(format!(...)) }`。
/// * **返回值空 ⇔ 每一项都成功** —— `rollback` 的 `SessionSnapshot::clear()`
///   正是挂在这个条件上（有失败就必须留着快照让下次启动重试）。
fn try_each<T>(
    items: impl IntoIterator<Item = T>,
    mut f: impl FnMut(T) -> Result<(), String>,
) -> Vec<String> {
    let mut failures = Vec::new();
    for item in items {
        if let Err(msg) = f(item) {
            failures.push(msg);
        }
    }
    failures
}

/// 按快照回滚。**尽力而为**：单项失败不影响其余项。
pub fn rollback(snap: &SessionSnapshot) -> Result<()> {
    // 0) **信任锚最先撤**：它是"我们额外加进系统钥匙串的信任"，越早收回越安全。
    //    并且**按备份记录**撤 —— 安装前就存在的证书不许删（那是用户自己的）。
    let mut failures = try_each(snap.trust_anchors.iter().rev(), |backup| {
        crate::macos::trust::rollback(backup)
            .map_err(|e| format!("移除信任锚 {} 失败: {e}", backup.fingerprint))
    });

    // 1) DNS 先还原（见模块文档里的顺序说明）
    failures.extend(try_each(snap.dns_backups.iter().rev(), |backup| {
        dns::restore(backup).map_err(|e| format!("还原 {} 的 DNS 失败: {e}", backup.service))
    }));

    // 2) 再倒序处理路由：**先删自己那条，再恢复被顶掉的那条**（task-85）。
    //
    // 「删除 ≠ 恢复」：`route add` 对同前缀是**替换**语义 —— 我们装
    // `127.0.0.0/8 → 物理网关` 会把内核那条 on-link 的 `127/8 → lo0` 顶掉，
    // 只删除的话内核原来那条**不会自己回来**（实测：`127.0.0.2` 从此永久丢包）。
    // 所以动作由 `plan::rollback_plan` 算：Delete +（当时记下了的）Restore。
    //
    // `pending_routes` 也要处理：两阶段启动期间崩溃时，我们无法确定它到底装上了没有，
    // 而 `route delete` 对「不存在」是安全的空操作（已显式忽略 not-in-table），
    // 所以「宁可多删一次」是这里唯一正确的策略。
    let all_routes: Vec<InstalledRoute> = snap
        .installed_routes
        .iter()
        .chain(snap.pending_routes.iter())
        .cloned()
        .collect();
    failures.extend(try_each(rollback_plan(&all_routes), |action| match action {
        RollbackAction::Delete { destination, via } => route::delete(&destination, &via)
            .map_err(|e| format!("删除路由 {destination} 失败: {e}")),
        RollbackAction::Restore { destination, via } => route::add(&destination, &via)
            .map_err(|e| format!("恢复路由 {destination}（原本经由 {via:?}）失败: {e}")),
    }));

    // 3) 数据面进程由调用方（helper）负责杀掉，这里只报告 pid
    if let Some(pid) = snap.datapath_pid {
        tracing::info!(pid, "回滚：请调用方终止数据面进程");
    }

    // 4) **复检**：发出恢复命令 ≠ 系统已回到用户原本的样子。
    //
    //    只在前面的动作**全部报成功**时才复检 —— 复检的唯一目的就是堵住
    //    **假成功**（命令都退出 0、快照被删掉，而系统其实没回去）。已经失败时
    //    照旧上报失败、保留快照，不必再往系统上多打几条只读命令。
    if failures.is_empty() {
        failures.extend(verify_rollback_took_effect(snap, !all_routes.is_empty()));
    }

    // 5) 全部成功才删快照；有失败就留着让下次启动重试。
    if failures.is_empty() {
        SessionSnapshot::clear()?;
        Ok(())
    } else {
        Err(Error::Invalid(failures.join("; ")))
    }
}

/// 回滚之后的**复检 + 一次重试**。
///
/// # 为什么不能只看命令退出码
///
/// `networksetup -setdnsservers` / `route delete` 返回 0 **不等于**配置真的变了：
/// 别的 VPN 工具可能同时写同一张表、命令可能在半路被杀、`route` 的 `-ifscope`
/// 语义也可能与预期不同。旧实现只要没报错就 `SessionSnapshot::clear()`，于是出现
/// 「用户没网、日志却说已回滚」——与 task-122 A-1 同一类**假结论**。
///
/// 这里只钉三条**用户可感知**的不变量（本卡的 P0 判据），任一条不满足 ⇒ 返回可读
/// 失败文案（由调用方汇总进 `Err`：快照保留、错误上传，**绝不假装成功**）：
///
/// 1. 见 [`verify_default_route_is_back`]：机器仍有**不在隧道上、带网关**的默认路由；
/// 2. 见 [`verify_capture_routes_are_gone`]：**本次会话的**捕获路由不许还挂在
///    `snap.interface` 那个 utun 上（否则全机流量会进一条即将被关掉的黑洞隧道）；
/// 3. 见 [`verify_dns_is_back`]：每个改过的服务，其 DNS **逐值等于**备份。
///
/// * `touched_routes`：本次回滚是否动过路由。没动过就不该拿路由表去判人家的死活
///   （否则会把「用户本来就没有默认路由」误算成我们的回滚失败）。
fn verify_rollback_took_effect(snap: &SessionSnapshot, touched_routes: bool) -> Vec<String> {
    let mut failures = Vec::new();
    if touched_routes {
        failures.extend(verify_default_route_is_back(snap));
        failures.extend(verify_capture_routes_are_gone(snap));
    }
    failures.extend(verify_dns_is_back(snap));
    failures
}

/// 判据 ①：回滚之后机器必须仍有一条**能用的**默认路由。
///
/// 「能用」= `route -n get default` 拿得到、**带网关**、且**不在 `utun` 上**。
/// 最后一条是「App 不在之后流量不该还挂在隧道里」的直接表达；前两条来自
/// `route.rs` 的实测（无网关的默认路由会退化成「目标在本地链路」的纯接口路由，
/// 包根本发不出去 —— 比没有更糟）。
///
/// 缺了就**按快照记下的物理网关重装一次**（重试一次）。只在「真的没有默认路由」
/// 时才补：那已经等于没网了，补一条只可能变好；而「查得到但不可用」（在隧道上 /
/// 没网关 / 读失败）**不猜**，只如实上报。
fn verify_default_route_is_back(snap: &SessionSnapshot) -> Vec<String> {
    if default_route_problem().is_none() {
        return Vec::new();
    }
    // 重试一次：只对「`route -n get default` 查不到」这一态做修复，且必须有 IPv4 网关。
    let missing = matches!(route::default_route(), Err(Error::NoDefaultRoute));
    if missing {
        if let Some(gw) = snap.physical.gateway.filter(|g| g.is_ipv4()) {
            let dest = Cidr {
                addr: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                prefix: 0,
            };
            match route::add(&dest, &RouteVia::Gateway { addr: gw }) {
                Ok(()) => tracing::warn!(
                    gateway = %gw,
                    "回滚复检：默认路由缺失，已按快照网关重装一次"
                ),
                Err(e) => tracing::error!(
                    gateway = %gw,
                    error = %e,
                    "回滚复检：默认路由缺失，重装失败"
                ),
            }
        } else {
            tracing::error!(
                interface = %snap.physical.interface,
                "回滚复检：默认路由缺失，但快照里没有可用的 IPv4 网关，不敢凭空造一条"
            );
        }
    }
    match default_route_problem() {
        None => Vec::new(),
        Some(problem) => vec![format!("回滚后默认路由未恢复: {problem}")],
    }
}

/// 读一次默认路由；`None` = 可用，`Some(人话)` = 出了什么问题。
fn default_route_problem() -> Option<String> {
    match route::default_route() {
        Ok(dr) if dr.interface.starts_with("utun") => {
            Some(format!("默认路由仍指向隧道接口 {}", dr.interface))
        }
        Ok(dr) if dr.gateway.is_none() => Some(format!(
            "默认路由在 {} 上但没有网关（目标在本地链路的纯接口路由，包发不出去）",
            dr.interface
        )),
        Ok(_) => None,
        Err(e) => Some(format!("{e}")),
    }
}

/// 判据 ①b：**本次会话自己的**捕获路由（`0/1` / `128/1`）不许还挂在
/// `snap.interface` 那个 utun 上。
///
/// 这是「整机没网」的真正入口：删漏一条 `0/1 → utunN`，调用方随后关掉那个 fd，
/// 全机的默认流量就进了一条**已死的隧道**（黑洞），而 `route -n get default`
/// 仍然答得出系统默认路由 —— 只看默认路由**发现不了**这一条。
///
/// 只认**快照里那个 utun 名**：用户机器上还跑着 Karing / Tailscale，别人家的
/// `0/1 → utunX` 不是我们的账，不能算成回滚失败（那正是「同一件事两个口径」）。
///
/// 读不到路由表（`netstat` 失败）时**不断言** —— 不把「读失败」当成「没删掉」。
fn verify_capture_routes_are_gone(snap: &SessionSnapshot) -> Vec<String> {
    // 快照里根本没记过「挂在本会话 utun 上的 /1 捕获路由」⇒ 不必去读表，
    // 也就不会对别的会话/别的工具误报。
    let had_our_capture = snap
        .installed_routes
        .iter()
        .chain(snap.pending_routes.iter())
        .any(|r| {
            r.destination.prefix == 1
                && matches!(&r.via, RouteVia::Interface { name } if name == &snap.interface)
        });
    if !had_our_capture {
        return Vec::new();
    }
    let Ok(audit) = route::current_route_audit(&snap.physical.interface) else {
        tracing::warn!(
            interface = %snap.physical.interface,
            "回滚复检：读不到路由表，无法确认本次会话的捕获路由是否已删（如实记录，不当作失败）"
        );
        return Vec::new();
    };
    let mut failures = Vec::new();
    for (label, seen) in [
        ("0/1", audit.capture_0_1),
        ("128/1", audit.capture_128_1),
    ] {
        if seen.as_deref() == Some(snap.interface.as_str()) {
            failures.push(format!(
                "回滚后捕获路由 {label} 仍指向本次会话的隧道 {}（全机流量会进已死的隧道）",
                snap.interface
            ));
        }
    }
    failures
}

/// 判据 ②：每个被我们改过 DNS 的服务，当前值必须**逐值（含顺序）等于**备份。
///
/// 为什么是逐值相等而不是「包含」：多一个隧道哨兵（`198.18.0.2`）就是整机解析不了
/// 域名；少一个用户自己设的服务器同样是坏的。**备份为空 = 原本走 DHCP ⇒ 现在也
/// 必须是空**（`networksetup` 的 `Empty` 语义）—— 这是最容易被漏掉、也最容易
/// 让用户「终端没网」的一态。
///
/// 第一次不符就**立刻重试一次** `dns::restore`（把缺失窗口缩到最小），仍不符才上报。
///
/// 期望值取 [`dns::DnsBackup::effective_servers`]（已剔哨兵）：磁盘上的历史快照可能
/// 被旧版污染，若拿污染值当期望，会出现「系统已恢复成 DHCP、复检却仍判失败」的死循环。
fn verify_dns_is_back(snap: &SessionSnapshot) -> Vec<String> {
    let mut failures = Vec::new();
    for backup in &snap.dns_backups {
        let want = backup.effective_servers();
        if matches!(dns::get_dns(&backup.service), Ok(now) if servers_equal(&now, &want)) {
            continue;
        }
        // 重试一次。
        if let Err(e) = dns::restore(backup) {
            failures.push(format!("回滚后重试还原 {} 的 DNS 失败: {e}", backup.service));
            continue;
        }
        match dns::get_dns(&backup.service) {
            Ok(now) if servers_equal(&now, &want) => {
                tracing::warn!(
                    service = %backup.service,
                    "回滚复检：DNS 第一次不符，重试后已还原"
                );
            }
            Ok(now) => failures.push(format!(
                "回滚后 {} 的 DNS 未还原: 期望 {want:?}, 实际 {now:?}",
                backup.service
            )),
            Err(e) => failures.push(format!(
                "回滚后无法复检 {} 的 DNS（读当前值失败）: {e}",
                backup.service
            )),
        }
    }
    failures
}

/// DNS 服务器列表的比较：逐值（含顺序）相等。两边都以 `IpAddr` 文本存，非法项丢弃。
fn servers_equal(now: &[String], want: &[String]) -> bool {
    let parse = |v: &[String]| -> Vec<IpAddr> { v.iter().filter_map(|s| s.parse().ok()).collect() };
    parse(now) == parse(want)
}

/// 拆除会话。`session_id` 不匹配时拒绝执行 —— 防止 GUI 的陈旧请求
/// 把我们后来建立的新会话误拆掉。
pub fn tear_down(session_id: &str) -> Result<SessionSnapshot> {
    let Some(mut snap) = SessionSnapshot::load()? else {
        return Err(Error::Invalid("没有正在运行的 TUN 会话".into()));
    };
    if snap.session_id != session_id {
        return Err(Error::Invalid(format!(
            "会话 id 不匹配（请求 {session_id}，当前 {}）",
            snap.session_id
        )));
    }

    snap.state = SessionState::TearingDown;
    snap.save()?;
    rollback(&snap)?;
    Ok(snap)
}

/// 只撤信任锚，不碰路由 / DNS。
///
/// 供 [`restore_stale`] 在"快照是 `Up`、但记录里还有信任锚"时使用：那种情况下
/// 路由/DNS 可能仍在生效（不该在启动时擅自拆一条活隧道），但那些根证书是上一个
/// 进程装的，必须收回来。
///
/// **尽力而为 + 失败保留记录**：撤成功的从记录里拿掉；撤失败的留在
/// `snap.trust_anchors` 里，下次启动（或本次会话的 `rollback`/`force_cleanup`）
/// 会重试 —— 不允许把失败悄悄吞掉（那正是"永久残留"的另一半原因）。
fn rollback_trust_anchors(snap: &mut SessionSnapshot) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    let anchors = std::mem::take(&mut snap.trust_anchors);
    let mut kept: Vec<_> = Vec::new();
    for backup in anchors.into_iter().rev() {
        match crate::macos::trust::rollback(&backup) {
            Ok(()) => {}
            Err(e) => {
                failures.push(format!("移除信任锚 {} 失败: {e}", backup.fingerprint));
                kept.push(backup);
            }
        }
    }
    // `kept` 是倒序收的，翻回来仍保持"安装顺序"的语义。
    kept.reverse();
    snap.trust_anchors = kept;
    // 记录必须立刻落盘：删成功的不能复活，删失败的不能在覆盖中丢失。
    snap.save()?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::Invalid(failures.join("; ")))
    }
}

/// 会话是否**真的还活着** —— 不是快照里那个 `state` 标志位，而是**此刻**的实况。
///
/// # 判据与理由
///
/// 判据：**快照里记的那个 utun 接口现在是否还存在**（`ifconfig -l` 解析，读不到时
/// 退回内核 `if_nametoindex`，见 [`netif::interface_exists`]）。
///
/// 为什么是这个：
///
/// * 快照是**上一个进程**写的，`state == Up` 只说明"当时"建起来了。App 退出/崩溃
///   后 utun 的 fd 关闭，内核随即删掉接口与挂在它上面的路由 —— 但磁盘上的快照
///   仍写着 `Up`。只看标志位就会把"已经死了的会话"当成"活会话"而跳过清理，
///   于是 DNS 停在我们自己的哨兵 `198.18.0.2` 上，用户终端整机解析不了域名。
/// * 接口存在 ⇔ 会话的数据面资源还在：`HandoffFd` 模式下 fd 在 App 手里，
///   App 在 ⇒ 接口在；`HelperSpawn` 模式下 fd 在 helper 手里，helper 重启 ⇒ 接口消失。
///   两种模式下"接口是否存在"都精确对应"会话是否还占着系统资源"。
/// * 它是**只读探测**：不断言、不改配置，失败也不会让情况变坏。
///
/// # 它挡不住什么（已知边界）
///
/// * **utun 名字复用**：若会话消失后又有别的进程建出同名的 `utunN`，会被误判成
///   "活"。macOS 的 utun 号是内核按可用号递增分配的，同名复现需要恰好回收同一个号，
///   概率低但非零 —— 所以这里只用于"决定要不要清 DNS"，误判的后果是**不动**
///   （宁可漏清，也不误拆一条在用的隧道），不会写坏用户配置。
/// * **helper 重启但 App 仍持有 fd**：接口仍在 ⇒ 判为活会话，启动时不拆。这正是
///   期望行为（数据面还在跑），但此时 helper 内存里已经没有这条会话，最终还要靠
///   App 自己退出时的 `force_cleanup` 或下一次启动对账。
/// * 探测本身读不到（`ifconfig` 失败且内核也答不出）⇒ [`SessionLiveness::Unknown`]，
///   调用方一律**不动**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionLiveness {
    /// 接口还在 ⇒ 会话仍占着系统资源。
    Alive,
    /// 接口已不存在 ⇒ 会话已死，快照是残留。
    Gone,
    /// 探测失败，无法断言（fail-open：不误拆）。
    Unknown(String),
}

/// 见 [`SessionLiveness`]。
pub fn session_liveness(snap: &SessionSnapshot) -> SessionLiveness {
    match netif::interface_exists(&snap.interface) {
        Ok(true) => SessionLiveness::Alive,
        Ok(false) => SessionLiveness::Gone,
        Err(e) => SessionLiveness::Unknown(e.to_string()),
    }
}

/// 启动对账的结果（供日志与测试观察）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DnsReconcileReport {
    /// 实际读过的服务名。
    pub checked: Vec<String>,
    /// 真的被恢复（哨兵已清除）的服务。
    pub restored: Vec<String>,
    /// 因仍有活会话而**刻意不动**的服务。
    pub skipped_live: Vec<String>,
    /// 探测/恢复失败与原因（绝不假装成功）。
    pub failures: Vec<String>,
}

impl DnsReconcileReport {
    /// 没有任何失败。
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }

    /// 这次对账有没有真的动过系统 DNS。
    pub fn changed_anything(&self) -> bool {
        !self.restored.is_empty()
    }
}

/// **启动即对账**：把「系统 DNS 停在我们自己的哨兵地址」这一残留清掉。
///
/// 规则（本卡 P0）：
///
/// * 观察到的会话**还活着** ⇒ **一律不动**（DNS 指向哨兵是这条活隧道的正常状态，
///   不许误拆）；
/// * 会话已死 / 没有快照 ⇒ 逐服务检查：当前值里含哨兵就恢复。恢复成什么？
///   * 快照里记过这个服务 ⇒ 恢复成 [`dns::DnsBackup::effective_servers`]（已剔哨兵）；
///   * 没记过（快照丢了/被重装删了）⇒ 备份按空处理 ⇒ **清成 DHCP**。
///
/// 只清**精确命中哨兵**的值：别的 DNS 配置一律不碰。每个服务恢复后立刻复检，
/// 失败写进报告并打日志（点名服务、期望值、实际值）。
///
/// 快照损坏时也照常对账（那时没有"活会话"的证据，按残留处理）—— 宁可直连。
pub fn reconcile_dns_residue() -> DnsReconcileReport {
    let mut report = DnsReconcileReport::default();

    let snap = match SessionSnapshot::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                error = %e,
                "启动对账：读取遗留会话快照失败，按「没有活会话」继续（只清哨兵）"
            );
            None
        }
    };

    // 有活会话 / 无法确认会话死活 ⇒ 都不动（fail-open：绝不误拆在用的隧道）。
    if let Some(s) = &snap {
        match session_liveness(s) {
            SessionLiveness::Alive => {
                report.skipped_live = s.dns_backups.iter().map(|b| b.service.clone()).collect();
                tracing::info!(
                    interface = %s.interface,
                    services = ?report.skipped_live,
                    "启动对账：会话仍在运行，DNS 一律不动（指向哨兵是正常状态）"
                );
                return report;
            }
            SessionLiveness::Unknown(why) => {
                tracing::warn!(
                    session = %s.session_id,
                    interface = %s.interface,
                    reason = %why,
                    "启动对账：无法确认快照里的 utun 是否还在；按「不误拆」处理，DNS 一处不动"
                );
                return report;
            }
            SessionLiveness::Gone => {}
        }
    }

    // 待查服务 = 快照里记过的（带原始值）∪ 所有启用的服务（快照丢了时的兜底）。
    let mut originals: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if let Some(s) = &snap {
        for b in &s.dns_backups {
            originals.insert(b.service.clone(), b.effective_servers());
        }
    }
    let mut services: Vec<String> = originals.keys().cloned().collect();
    match dns::list_services() {
        Ok(all) => {
            for s in all {
                if !services.contains(&s) {
                    services.push(s);
                }
            }
        }
        Err(e) => report.failures.push(format!(
            "列出网络服务失败，无法确认是否还有服务停在哨兵上: {e}"
        )),
    }

    for service in services {
        report.checked.push(service.clone());
        let now = match dns::get_dns(&service) {
            Ok(v) => v,
            Err(e) => {
                report.failures.push(format!(
                    "读取「{service}」的 DNS 失败，无法判断是否残留哨兵: {e}"
                ));
                continue;
            }
        };
        if !dns::has_sentinel(&now) {
            continue;
        }
        let want = originals.get(&service).cloned().unwrap_or_default();
        tracing::warn!(
            service = %service,
            expected = ?want,
            actual = ?now,
            "启动对账：系统 DNS 停在我们自己的哨兵地址且会话已不在，正在恢复"
        );
        match dns::restore_servers(&service, &want) {
            // 命令成功 ≠ 真的写进去了：立刻复检一次。
            Ok(()) => match dns::get_dns(&service) {
                Ok(after) if !dns::has_sentinel(&after) => {
                    report.restored.push(service.clone());
                    tracing::warn!(service = %service, now = ?after, "启动对账：已恢复（哨兵已清除）");
                }
                Ok(after) => report.failures.push(format!(
                    "启动对账后「{service}」的 DNS 仍是 {after:?}（期望 {want:?}，原值 {now:?}）"
                )),
                Err(e) => report.failures.push(format!(
                    "启动对账后无法复检「{service}」的 DNS: {e}"
                )),
            },
            Err(e) => report.failures.push(format!(
                "启动对账恢复「{service}」的 DNS 失败（期望 {want:?}，实际 {now:?}）: {e}"
            )),
        }
    }
    report
}

/// **启动恢复入口**（helper 的 `recover_from_crash` 调用）。
///
/// 两件事，顺序固定：
///
/// 1. **回滚上次遗留的会话** —— 判据是「会话现在是否真的还在」（[`session_liveness`]），
///    不是快照里的 `state`。接口已不存在 ⇒ 按残留回滚（DNS 先于路由还原）。
/// 2. **DNS 哨兵对账** —— 覆盖"回滚没能碰到"的残留：没有快照、备份为空、
///    或备份本身被哨兵污染（见 [`reconcile_dns_residue`]）。
///
/// 信任锚仍然与会话状态解耦、无条件撤销（P0-2）。有活会话时**只**撤信任锚，
/// 路由/DNS 一律不动。
pub fn restore_stale() -> Result<Option<SessionSnapshot>> {
    let snap = match SessionSnapshot::load() {
        Ok(Some(s)) => s,
        Ok(None) => {
            // 没有快照也要对账：DNS 可能停在我们自己的哨兵上而快照已被删（重装/卸载残留）。
            let report = reconcile_dns_residue();
            return if report.is_clean() {
                Ok(None)
            } else {
                Err(Error::Invalid(report.failures.join("; ")))
            };
        }
        Err(e) => {
            // 快照损坏：没有"活会话"的证据，按残留对账（宁可直连），并把读失败如实上报。
            tracing::error!(error = %e, "读取遗留会话快照失败；跳过回滚，只对账 DNS 哨兵");
            let report = reconcile_dns_residue();
            let mut failures = vec![format!("读取遗留会话快照失败: {e}")];
            failures.extend(report.failures);
            return Err(Error::Invalid(failures.join("; ")));
        }
    };
    let mut snap = snap;

    // ---- 信任锚：与会话状态**解耦**（P0-2）----
    //
    // helper 被 `kill -9` / 机器重启后，磁盘上的快照可能仍是 `Up`：上个进程已经
    // 不在了，但它装进钥匙串的根证书还在。旧实现只看 `is_stale()`，于是这条记录
    // 被跳过；下一次 `TunUp` 又整份覆盖快照 ⇒ 证书永久残留、界面再也删不掉。
    //
    // 能走到这里的只有**启动路径**（`recover_from_crash`）—— 也就是说这份快照
    // 一定来自上一个进程。所以只要记着信任锚就无条件撤掉，与 is_stale 无关。
    let mut anchor_failure: Option<Error> = None;
    if snap.has_trust_anchors() {
        tracing::warn!(
            session = %snap.session_id,
            state = ?snap.state,
            anchors = snap.trust_anchors.len(),
            "发现上个进程遗留的信任锚记录：无条件撤销（与 is_stale 无关）"
        );
        if let Err(e) = rollback_trust_anchors(&mut snap) {
            anchor_failure = Some(e);
        }
    }

    // ---- 判据：会话现在是不是**真的还在**，而不是快照里那个 state 标志位 ----
    // 只探一次：探测结果直接用在下判断与日志里（重复探测既浪费又可能自相矛盾）。
    let liveness = session_liveness(&snap);
    match &liveness {
        SessionLiveness::Alive if !snap.is_stale() => {
            // 接口还在、也没崩在半路 ⇒ 真有一条活隧道。启动时不拆它：
            // 路由/DNS 交给它自己（或 GUI 的 force_cleanup）收尾。
            tracing::info!(
                session = %snap.session_id,
                interface = %snap.interface,
                "启动检查：快照里的会话仍在运行（接口存在），不拆；只撤信任锚"
            );
            return match anchor_failure {
                Some(e) => Err(e),
                None => Ok(None),
            };
        }
        SessionLiveness::Unknown(why) => {
            tracing::warn!(
                session = %snap.session_id,
                interface = %snap.interface,
                reason = %why,
                "启动检查：无法确认快照里的 utun 是否还在；按「不误拆」处理，不动路由/DNS"
            );
            return match anchor_failure {
                Some(e) => Err(e),
                None => Ok(None),
            };
        }
        // 接口已不存在（Gone）⇒ 残留；或崩在半路（is_stale）⇒ 必须回滚。
        _ => {}
    }

    let why = match &liveness {
        SessionLiveness::Gone => format!("接口 {} 已不存在（会话已死）", snap.interface),
        _ => "会话崩在半路".to_string(),
    };
    tracing::warn!(
        session = %snap.session_id,
        interface = %snap.interface,
        routes = snap.installed_routes.len(),
        reason = %why,
        "发现未清理的 TUN 会话，正在回滚并做 DNS 对账"
    );

    let rolled = rollback(&snap);
    let report = reconcile_dns_residue();

    let mut failures: Vec<String> = Vec::new();
    if let Err(e) = rolled {
        failures.push(format!("回滚遗留会话失败: {e}"));
    }
    failures.extend(report.failures.iter().cloned());
    if let Some(e) = anchor_failure {
        failures.push(format!("撤销信任锚失败: {e}"));
    }
    if failures.is_empty() {
        Ok(Some(snap))
    } else {
        Err(Error::Invalid(failures.join("; ")))
    }
}

/// 无论快照状态如何都强制清理（用于「一键修复网络」）。
pub fn force_cleanup() -> Result<Option<SessionSnapshot>> {
    let Some(snap) = SessionSnapshot::load()? else {
        return Ok(None);
    };
    tracing::warn!(session = %snap.session_id, "强制清理遗留会话");
    // **回滚失败必须往上抛，不许在源头吞掉**（task-122 A-1）。
    //
    // 以前这里是 `let _ = rollback(&snap); Ok(Some(snap))` ⇒ 调用方（helper 的
    // `Request::Restore`）里那个 `Err` 分支**不可达** ⇒ 删路由/还原 DNS 任一步
    // 失败，用户看到的仍是「已回滚会话 …」，而界面文案承诺「网络会回到直连」。
    // 这就是 A 级判据：**用户据此得出了错误结论**。
    //
    // 安全性：`rollback` 只在**全部步骤成功**时删快照（失败会留着让下次启动重试），
    // 所以这里 `?` 既让错误可见，也不破坏「失败可重试」。
    rollback(&snap)?;
    Ok(Some(snap))
}

fn parse_utun_unit(name: &str) -> Option<u32> {
    name.strip_prefix("utun").and_then(|n| n.parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **task-122 A-1 守卫**：`force_cleanup` 不许吞回滚错误。
    ///
    /// 吞掉的后果不是"少一条日志"，而是**调用方的 Err 分支不可达** ——
    /// 用户会看到「已回滚会话 …」，而网络其实没回去。
    #[test]
    fn force_cleanup_propagates_rollback_errors_in_production_source() {
        let prod = include_str!("controller.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        // **先去掉行注释**：这条守卫第一次跑就红在它自己的解释性注释上
        // （注释里引用了旧的 `let _ = rollback(...)` 写法）—— 守卫自己假红，
        // 正是 task-75 那次"文本守卫"的同一个坑。（本文件这几行里没有
        // 字符串字面量含 `//`，所以按行截断足够。）
        let code = prod
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let body = code
            .split("pub fn force_cleanup")
            .nth(1)
            .and_then(|r| r.split("\nfn ").next())
            .unwrap_or("");
        assert!(!body.is_empty(), "没找到 force_cleanup 的函数体（锚点变了？）");
        assert!(
            body.contains("rollback(&snap)?"),
            "force_cleanup 必须把回滚失败往上抛（`rollback(&snap)?`）"
        );
        assert!(
            !body.contains("let _ = rollback"),
            "不许再把回滚错误吞掉 —— 那会让 server.rs 的 Err 分支不可达"
        );
    }

    #[test]
    fn parses_utun_unit_from_name() {
        assert_eq!(parse_utun_unit("utun0"), Some(0));
        assert_eq!(parse_utun_unit("utun17"), Some(17));
        assert_eq!(parse_utun_unit("en0"), None);
        assert_eq!(parse_utun_unit("utunx"), None);
    }

    #[test]
    fn resolve_via_fills_placeholder_interface() {
        let via = RouteVia::Interface { name: String::new() };
        assert_eq!(
            resolve_via(&via, "utun7"),
            RouteVia::Interface { name: "utun7".into() }
        );
    }

    #[test]
    fn resolve_via_keeps_explicit_interface() {
        let via = RouteVia::Interface { name: "en0".into() };
        assert_eq!(resolve_via(&via, "utun7"), via);
    }

    #[test]
    fn resolve_via_keeps_gateway() {
        let via = RouteVia::Gateway { addr: "192.168.1.1".parse().unwrap() };
        assert_eq!(resolve_via(&via, "utun7"), via);
    }

    #[test]
    fn rejects_empty_address_list_before_touching_system() {
        let req = TunUpRequest {
            session_id: "s".into(),
            interface_name: None,
            mtu: 1500,
            addresses: vec![],
            routes: xt_proto::RoutePlan {
                default_route: xt_proto::DefaultRouteMode::SplitDefault,
                bypass_hosts: vec![],
                bypass_networks: vec![],
                ipv6: xt_proto::Ipv6Mode::Passthrough,
            },
            dns: xt_proto::DnsPlan {
                mode: DnsMode::Automatic,
                servers: vec![],
                search_domains: vec![],
            },
            datapath: DatapathPlan::HandoffFd,
            defer_default_routes: false,
        };
        // 这个断言在非 root 环境下也成立：校验发生在创建 utun 之前。
        assert!(bring_up(&req).is_err());
    }

    #[test]
    fn rejects_absurd_mtu() {
        let req = TunUpRequest {
            session_id: "s".into(),
            interface_name: None,
            mtu: 10,
            addresses: vec!["198.18.0.1/15".parse().unwrap()],
            routes: xt_proto::RoutePlan {
                default_route: xt_proto::DefaultRouteMode::SplitDefault,
                bypass_hosts: vec![],
                bypass_networks: vec![],
                ipv6: xt_proto::Ipv6Mode::Passthrough,
            },
            dns: xt_proto::DnsPlan {
                mode: DnsMode::Automatic,
                servers: vec![],
                search_domains: vec![],
            },
            datapath: DatapathPlan::HandoffFd,
            defer_default_routes: false,
        };
        assert!(bring_up(&req).is_err());
    }

    // -----------------------------------------------------------------------
    // task-134：A-1 的**行为级**接缝测试（注入失败的路由执行器）
    // -----------------------------------------------------------------------

    fn tmp_snapshot_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xt-tun-snap-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时快照根");
        dir
    }

    fn fixture_uplink() -> PhysicalUplink {
        PhysicalUplink {
            interface: "en0".into(),
            gateway: Some("192.168.0.1".parse().unwrap()),
            service: None,
        }
    }

    fn fixture_route(destination: &str) -> InstalledRoute {
        InstalledRoute {
            destination: destination.parse().unwrap(),
            via: RouteVia::Interface {
                name: "utun9".into(),
            },
            replaced: None,
        }
    }

    /// **task-134 行为级接缝测试**：真失败时 `force_cleanup()` 必须
    ///
    /// * 返回 **`Err`**（不是 `Ok(Some(..))`）—— 旧实现 `let _ =` 会吞掉；
    /// * **快照留着**（失败可重试语义仍在）；
    /// * 文案含**具体失败步骤**，且**不含「已回滚」**；
    /// * 修好后**重试能成功**、成功才删快照（同一条测试里行为级验证）。
    ///
    /// 注入的替身让**第二步**失败 —— 这正是会被静默吞掉的那条路径。
    /// 注入钩子是 `#[cfg(test)]` 的（见 `macos::with_executor`、`snapshot::with_test_root`），
    /// **生产路径一行都没动**。
    #[test]
    fn force_cleanup_returns_err_keeps_snapshot_and_names_the_failed_step() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;
        use std::cell::Cell;
        use std::rc::Rc;

        let root = tmp_snapshot_root("a1-fail");
        let mut snap = SessionSnapshot::new("s-134".into(), "utun9".into(), fixture_uplink());
        // 回滚动作 = 倒序删除：先 198.51.100.0/24，再 203.0.113.0/24（第二步）。
        snap.installed_routes = vec![
            fixture_route("203.0.113.0/24"),
            fixture_route("198.51.100.0/24"),
        ];
        with_test_root(&root, || snap.save()).expect("写快照");

        let calls = Rc::new(Cell::new(0u32));
        let counter = calls.clone();
        let failing: crate::macos::TestExecutor =
            Rc::new(move |program: &str, args: &[String]| {
                counter.set(counter.get() + 1);
                if counter.get() == 2 {
                    Err(Error::Invalid(format!(
                        "注入：{program} {} 失败",
                        args.join(" ")
                    )))
                } else {
                    Ok(String::new())
                }
            });

        let (err_msg, kept) = with_test_root(&root, || {
            let result = with_executor(failing, force_cleanup);
            let kept = SessionSnapshot::snapshot_path().exists();
            let msg = result
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| format!("_ = {result:?} —— 必须返回 Err，不许吞"));
            (msg, kept)
        });

        assert!(kept, "回滚失败时快照必须留着（否则失败不可重试）");
        assert!(err_msg.contains("失败"), "文案要说清是失败：{err_msg}");
        assert!(
            err_msg.contains("203.0.113.0/24"),
            "文案要含**具体失败步骤**（第二步那条路由）：{err_msg}"
        );
        assert!(
            !err_msg.contains("已回滚"),
            "失败时绝不许出现「已回滚」——那正是 A-1 要修的假结论：{err_msg}"
        );
        assert_eq!(calls.get(), 2, "只该跑到失败的那一步为止");

        // 「失败可重试」也要行为级成立：换一个**系统已还原**的执行器再跑一次 ⇒ Ok，
        // 且这时才删快照。
        //
        // ⚠️ 它必须也回答回滚后的复检探针（`route -n get default` 要返回一条真实
        // 形态的默认路由）——「所有命令都返回空串」现在会被复检**正确地**判成
        // 「没有默认路由」。这正是本卡新增的那条不变量在起作用。
        let ok: crate::macos::TestExecutor = dispatch(HEALTHY_DEFAULT_ROUTE, "");
        let (retry, gone) = with_test_root(&root, || {
            let r = with_executor(ok, force_cleanup);
            (
                r.map(|o| o.is_some()),
                !SessionSnapshot::snapshot_path().exists(),
            )
        });
        assert_eq!(
            retry.as_ref().ok(),
            Some(&true),
            "修好后重试必须成功（失败是暂时的）：{retry:?}"
        );
        assert!(gone, "只有全部成功才允许删快照");
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // P0-2：孤儿信任锚不许被会话覆盖吞掉
    //
    // 场景：helper 在会话 `Up` 时被 kill -9 / 机器重启。磁盘快照仍是 `Up`
    // （`is_stale()` == false），而根证书已经在 System.keychain 里。
    // 修前：`restore_stale()` 直接跳过；下一次 `TunUp` 又用
    // `SessionSnapshot::new(...).save()` 整份覆盖快照 ⇒ 指纹记录消失、
    // 界面再也删不掉那张证书。
    // -----------------------------------------------------------------------

    const ORPHAN_FP: &str = "AB:CD:EF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00";

    fn orphan_backup() -> crate::macos::trust::TrustAnchorBackup {
        crate::macos::trust::TrustAnchorBackup {
            fingerprint: ORPHAN_FP.into(),
            cert_path: "/nonexistent/orphan-ca.pem".into(),
            existed_before: false,
        }
    }

    /// 旧会话：`Up`（`is_stale()` == false）+ 一条信任锚记录。
    fn save_orphaned_up_session(root: &std::path::Path) {
        use crate::macos::snapshot::with_test_root;
        let mut old = SessionSnapshot::new("old-session".into(), "utun3".into(), fixture_uplink());
        old.state = SessionState::Up;
        old.trust_anchors.push(orphan_backup());
        assert!(!old.is_stale(), "前提：旧会话自认 Up、不算崩在半路");
        with_test_root(root, || old.save()).expect("写旧快照");
    }

    /// `ifconfig -l` 固定回答。
    ///
    /// `restore_stale()` 现在靠「接口是否还在」判活，测试必须把这一步确定化 ——
    /// 否则结果会随真机上恰好有哪些 `utunN` 而变（现场这台机器有 utun0..utun6）。
    fn ifconfig_listing(listing: &'static str) -> crate::macos::TestExecutor {
        std::rc::Rc::new(move |program: &str, args: &[String]| {
            if program == crate::tools::IFCONFIG && argv_is(args, &["-l"]) {
                return Ok(listing.to_string());
            }
            Ok(String::new())
        })
    }

    /// 记录 `security(1)` 收到的每一条命令；不碰真钥匙串。
    fn recording_stub(
        ok: bool,
        stderr: &'static str,
    ) -> (
        crate::macos::trust::SecurityStub,
        std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        let seen: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&seen);
        let stub: crate::macos::trust::SecurityStub =
            Rc::new(move |args: &[String]| {
                sink.borrow_mut().push(args.to_vec());
                (ok, Vec::new(), stderr.as_bytes().to_vec())
            });
        (stub, seen)
    }

    fn saw_delete_of(seen: &[Vec<String>], fingerprint: &str) -> bool {
        seen.iter().any(|args| {
            args.first().map(String::as_str) == Some("delete-certificate")
                && args.iter().any(|a| a == fingerprint)
        })
    }

    /// **核心判别测试**：旧会话是 `Up`，helper 重启后 `restore_stale()` 也必须
    /// 把它的信任锚撤掉。
    ///
    /// 修前：`!is_stale()` ⇒ 直接 `Ok(None)`，一条 `security delete-certificate`
    /// 都不会发出 ⇒ 证书永久留在钥匙串。（这条测试在旧代码上 `seen` 为空。）
    #[test]
    fn restore_stale_revokes_an_up_sessions_orphaned_trust_anchor() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::trust::with_security_stub;

        let root = tmp_snapshot_root("p02-restore-up");
        save_orphaned_up_session(&root);

        let (stub, seen) = recording_stub(true, "");
        let (result, left, cmds) = with_test_root(&root, || {
            // 明确声明 utun3 还在 ⇒ 这是「活会话」，启动时不拆（只撤锚）。
            let r = crate::macos::with_executor(ifconfig_listing("lo0 en0 utun3"), || {
                with_security_stub(stub, restore_stale)
            });
            let left = SessionSnapshot::load().ok().flatten();
            (r.map(|o| o.is_some()), left, seen.borrow().clone())
        });

        assert!(
            !result.expect("撤销孤儿锚不该失败"),
            "Up 会话不在启动时整体拆掉（路由/DNS 可能仍在生效）"
        );
        assert!(
            saw_delete_of(&cmds, ORPHAN_FP),
            "旧指纹必须真的被交给 `security delete-certificate`：{cmds:?}"
        );
        let left = left.expect("快照文件本身仍在（会话记录没被清）");
        assert!(left.trust_anchors.is_empty(), "撤成功的锚不许留在记录里");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 判别测试：模拟「旧会话 `Up` + 新会话建立」之后，旧指纹仍然
    /// **可被列出**（快照里还在）且**可被删除**（回滚新会话时真的下发 delete）。
    #[test]
    fn a_new_session_adopts_the_previous_trust_anchor_so_it_stays_revocable() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::trust::with_security_stub;

        let root = tmp_snapshot_root("p02-adopt");
        save_orphaned_up_session(&root);

        // 先跑一遍**旧行为**（裸 `SessionSnapshot::new` + `save`）作为对照：
        // 它确实会当场把记录覆盖掉 —— 这正是要修的病。
        {
            let mut overwritten =
                SessionSnapshot::new("new-session".into(), "utun4".into(), fixture_uplink());
            with_test_root(&root, || overwritten.save()).expect("旧行为写快照");
            let after = with_test_root(&root, SessionSnapshot::load)
                .expect("读快照")
                .expect("快照仍在");
            assert!(
                after.trust_anchors.is_empty(),
                "对照：旧写法确实丢记录（不是理论，是这一步真的发生了）"
            );
        }
        // 恢复现场，再走新路径。
        save_orphaned_up_session(&root);

        // 新会话建立（`bring_up` 里的那一步）。
        let mut fresh = with_test_root(&root, || {
            new_snapshot_adopting_leftover_anchors(
                "new-session".into(),
                "utun4".into(),
                fixture_uplink(),
            )
        })
        .expect("建新快照");
        assert_eq!(fresh.session_id, "new-session");
        assert!(
            fresh.trust_anchors.iter().any(|b| b.fingerprint == ORPHAN_FP),
            "旧指纹必须被带进新会话；否则那张根证书再也删不掉"
        );
        with_test_root(&root, || fresh.save()).expect("写新快照");

        // 「可被列出」：读快照就能看到旧指纹。
        let listed = with_test_root(&root, SessionSnapshot::load)
            .expect("读快照")
            .expect("快照应当存在");
        assert!(
            listed.trust_anchors.iter().any(|b| b.fingerprint == ORPHAN_FP),
            "旧指纹必须可被列出"
        );

        // 「可被删除」：对新会话走 force_cleanup（GUI「修复网络」/「退出」那条路），
        // 用替身捕获命令行 —— 必须看到 `delete-certificate -Z <旧指纹>`。
        let (stub, seen) = recording_stub(true, "");
        let rolled = with_test_root(&root, || with_security_stub(stub, force_cleanup));
        assert!(rolled.is_ok(), "回滚新会话应当成功: {rolled:?}");
        assert!(
            saw_delete_of(&seen.borrow(), ORPHAN_FP),
            "旧指纹必须被真的交给 `security delete-certificate`：{:?}",
            seen.borrow()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 撤销失败时：**记录留在快照里**（下次还能重试），并且错误必须往上传
    /// （`recover_from_crash` 会记 error）—— 不许静默、不许吞。
    #[test]
    fn restore_stale_keeps_the_anchor_record_when_revocation_fails() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::trust::with_security_stub;

        let root = tmp_snapshot_root("p02-retry");
        save_orphaned_up_session(&root);

        let (stub, _seen) = recording_stub(false, "SecKeychain: permission denied");
        let (result, left) = with_test_root(&root, || {
            // utun3 仍在 ⇒ 活会话：启动时不动路由/DNS，但锚的失败必须照旧上报。
            let r = crate::macos::with_executor(ifconfig_listing("lo0 en0 utun3"), || {
                with_security_stub(stub, restore_stale)
            });
            (r, SessionSnapshot::load().ok().flatten())
        });

        let err = result.expect_err("撤销失败必须往上抛，不许静默");
        assert!(err.to_string().contains(ORPHAN_FP), "错误要点出指纹：{err}");
        let left = left.expect("快照仍在");
        assert!(
            left.trust_anchors.iter().any(|b| b.fingerprint == ORPHAN_FP),
            "失败的锚必须留在记录里以便重试"
        );
        assert!(!left.is_stale(), "这条会话本身不算崩在半路");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 文本守卫：`bring_up` 不许再裸用 `SessionSnapshot::new`。
    ///
    /// 裸用 = 新会话建立时整份覆盖旧快照 = 旧信任锚记录当场丢失（P0-2）。
    #[test]
    fn bring_up_never_overwrites_a_snapshot_without_adopting_anchors() {
        let prod = include_str!("controller.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or("");
        // 先去掉行注释：本文件的解释性注释里引用了裸写法。
        let code = prod
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        let body = code
            .split("pub fn bring_up")
            .nth(1)
            .and_then(|r| r.split("\nfn ").next())
            .unwrap_or("");
        assert!(!body.is_empty(), "没找到 bring_up 的函数体（锚点变了？）");
        assert!(
            body.contains("new_snapshot_adopting_leftover_anchors"),
            "bring_up 必须通过 adopting 构造函数建快照"
        );
        assert!(
            !body.contains("SessionSnapshot::new"),
            "bring_up 不许裸建快照 —— 那会整份覆盖旧快照、丢掉旧信任锚记录"
        );
    }

    /// **task-25 判据（三条）**：回滚失败时，失败字符串必须**指名道姓**地把
    /// 「哪个信任锚（指纹）」「哪个网络服务（DNS）」「哪条路由（目的地）」
    /// 写出来 —— 界面/日志据此才能告诉用户"哪一步没退干净"。
    ///
    /// 这是抽 `try_each` 之前**先立**的判据（它是这次重构唯一的等价性依据）：
    /// 三条断言针对的是**当前实现**的行为，抽函数后必须逐字不变。
    #[test]
    fn rollback_failure_text_names_the_anchor_dns_service_and_route() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::trust::with_security_stub;
        use crate::macos::with_executor;

        const FP: &str = "AB:CD:EF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00";
        const SERVICE: &str = "Wi-Fi";
        const DEST: &str = "203.0.113.0/24";

        let root = tmp_snapshot_root("rollback-texts");
        let mut snap = SessionSnapshot::new("s-text".into(), "utun9".into(), fixture_uplink());
        snap.trust_anchors.push(crate::macos::trust::TrustAnchorBackup {
            fingerprint: FP.into(),
            cert_path: "/nonexistent/rollback-text-ca.pem".into(),
            existed_before: false,
        });
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: SERVICE.into(),
            servers: vec!["1.1.1.1".into()],
            search_domains: vec![],
        });
        // 无 `replaced` ⇒ 回滚动作只有 Delete（不引入 Restore 的额外文案）。
        snap.installed_routes = vec![fixture_route(DEST)];
        with_test_root(&root, || snap.save()).expect("写快照");

        // 三个来源同时失败：security 替身失败（stderr 不是"not found"）+
        // 外部命令替身全失败（DNS 与路由都走 `macos::run`）。
        let security: crate::macos::trust::SecurityStub = std::rc::Rc::new(
            |_args: &[String]| (false, Vec::new(), b"SecKeychain: permission denied".to_vec()),
        );
        let exec: crate::macos::TestExecutor =
            std::rc::Rc::new(|_p: &str, _a: &[String]| Err(Error::Invalid("注入：命令失败".into())));

        let (err_msg, kept) = with_test_root(&root, || {
            let r = with_executor(exec, || {
                with_security_stub(security, || rollback(&snap))
            });
            let kept = SessionSnapshot::snapshot_path().exists();
            let msg = r
                .as_ref()
                .err()
                .map(|e| e.to_string())
                .unwrap_or_else(|| format!("_ = {r:?} —— 有失败就必须 Err"));
            (msg, kept)
        });

        assert!(kept, "有失败时快照必须留着（失败可重试）");
        // 判据 1/3：信任锚 —— 指纹必须在文案里。
        assert!(
            err_msg.contains("移除信任锚") && err_msg.contains(FP),
            "锚指纹必须出现在失败文案里：{err_msg}"
        );
        // 判据 2/3：DNS —— 服务名必须在文案里。
        assert!(
            err_msg.contains(SERVICE) && err_msg.contains("DNS"),
            "DNS 服务名必须出现在失败文案里：{err_msg}"
        );
        // 判据 3/3：路由 —— 目的地必须在文案里。
        assert!(
            err_msg.contains(DEST) && err_msg.contains("路由"),
            "路由目的地必须出现在失败文案里：{err_msg}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `try_each` 自身的契约：按序逐项调用、收集**全部**失败文案、全成功返回空。
    ///
    /// 这三条正是从三处循环里搬走的语义 —— 尤其是「空 ⇔ 全成功」，
    /// `rollback` 的 `SessionSnapshot::clear()` 条件就挂在它上面。
    #[test]
    fn try_each_preserves_order_and_collects_every_failure() {
        let mut calls = Vec::new();
        let failures = try_each(1..=3, |n| {
            calls.push(n);
            Err(format!("第 {n} 项失败"))
        });
        assert_eq!(calls, vec![1, 2, 3], "必须按迭代顺序逐项调用，且不因失败中断");
        assert_eq!(
            failures,
            vec!["第 1 项失败", "第 2 项失败", "第 3 项失败"],
            "失败文案按调用顺序原样收集"
        );

        let mut reversed = Vec::new();
        let none = try_each((1..=3).rev(), |n| {
            reversed.push(n);
            Ok(())
        });
        assert!(none.is_empty(), "全部成功 ⇒ 返回空（rollback 才允许 clear 快照）");
        assert_eq!(
            reversed,
            vec![3, 2, 1],
            "`.rev()` 的方向由调用方决定，helper 不重排"
        );
    }

    // -----------------------------------------------------------------------
    // P0（本次卡）：**回滚之后的复检** —— 「发出恢复命令」≠「系统已回到原样」
    //
    // 现场（用户机器 `app.jsonl`，09-27 10:49:24，两次失败尝试都是这两行）：
    //   路由审计[TunUp 之后（接管前）]：en0 的作用域默认路由**在**；default 行 2 条
    //   路由审计[回滚之后]：en0 的作用域默认路由**缺失**；default 行 1 条
    //
    // 判据钉的是**用户可感知**的两条不变量（不是「我们发过命令」）：
    //   ① 回滚之后 `route::default_route()` 必须仍能拿到一条**不在隧道上、带网关**
    //      的默认路由（机器有出口）；
    //   ② 每个改过 DNS 的服务，其当前值必须**逐值等于**备份 —— 含「原本没有设置
    //      任何 DNS 服务器」（备份为空 = 交还 DHCP）这一态。
    //
    // ⚠️ 这两条在**改前**是红的：旧 `rollback` 只要命令不报错就 `clear()` 快照，
    // 从不回头读一眼系统 —— 于是「用户没网」而日志说「已回滚」（task-122 A-1 家族）。
    // -----------------------------------------------------------------------

    /// 一条**可用**默认路由的 `route -n get default` 真实形态（en0 + 网关）。
    const HEALTHY_DEFAULT_ROUTE: &str = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.0.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";

    fn argv_is(args: &[String], want: &[&str]) -> bool {
        args.len() == want.len() && args.iter().zip(want).all(|(a, b)| a == b)
    }

    /// 按 argv 分派的替身执行器：只对两个**复检读**给固定回答，其余命令一律「成功」。
    ///
    /// 这样测试能精确表达「命令都成功了，但系统其实没回到原样」这一形状 ——
    /// 而这正是旧实现会误报成功的地方。
    fn dispatch(route_get_default: &'static str, get_dns: &'static str) -> crate::macos::TestExecutor {
        std::rc::Rc::new(move |program: &str, args: &[String]| {
            if program == crate::tools::ROUTE && argv_is(args, &["-n", "get", "default"]) {
                return Ok(route_get_default.to_string());
            }
            if program == crate::tools::NETWORKSETUP
                && args.first().map(String::as_str) == Some("-getdnsservers")
            {
                return Ok(get_dns.to_string());
            }
            Ok(String::new())
        })
    }

    /// **判据 ①（改前红）**：复检发现「回滚后没有可用默认路由」⇒ 必须 `Err` 且保留快照。
    ///
    /// 旧实现根本不看默认路由 ⇒ 这里会拿到 `Ok`（快照被删），断言当场红。
    #[test]
    fn rollback_refuses_to_claim_success_when_the_default_route_is_gone() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;

        let root = tmp_snapshot_root("p0-default-route");
        let mut snap = SessionSnapshot::new("s-route".into(), "utun9".into(), fixture_uplink());
        snap.installed_routes = vec![fixture_route("203.0.113.0/24")];
        with_test_root(&root, || snap.save()).expect("写快照");

        // 空输出 ⇒ `parse_route_get` 拿不到 interface ⇒ `NoDefaultRoute`。
        let exec = dispatch("", "");
        let (result, kept) = with_test_root(&root, || {
            let r = with_executor(exec, || rollback(&snap));
            (r.map(|_| ()), SessionSnapshot::snapshot_path().exists())
        });

        let err = result.expect_err(
            "默认路由没回来 ⇒ rollback 必须返回 Err（不许「命令发出去了」就算成功）",
        );
        assert!(
            err.to_string().contains("默认路由"),
            "失败文案要点明是「默认路由」这一项：{err}"
        );
        assert!(kept, "复检失败时快照必须留着（失败可重试语义）");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **判据 ②（改前红）**：复检发现「回滚后 DNS 仍是哨兵」⇒ 必须 `Err`、点名服务与实际值。
    ///
    /// **含「原本没有设置任何 DNS 服务器」这一态**（备份为空 = 交还 DHCP）——
    /// 这正是用户机器上 `Wi-Fi` 的原值形态。
    #[test]
    fn rollback_refuses_to_claim_success_when_dns_is_still_the_sentinel() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;

        let root = tmp_snapshot_root("p0-dns-leftover");
        let mut snap = SessionSnapshot::new("s-dns".into(), "utun9".into(), fixture_uplink());
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec![],
            search_domains: vec![],
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        // 还原命令一律「成功」，但系统上仍然是隧道内的哨兵地址。
        let exec = dispatch(HEALTHY_DEFAULT_ROUTE, "198.18.0.2\n");
        let (result, kept) = with_test_root(&root, || {
            let r = with_executor(exec, || rollback(&snap));
            (r.map(|_| ()), SessionSnapshot::snapshot_path().exists())
        });

        let msg = result
            .expect_err("DNS 还指着 198.18.0.2 ⇒ 不许报成功")
            .to_string();
        assert!(
            msg.contains("Wi-Fi") && msg.contains("DNS"),
            "要点名服务名与 DNS：{msg}"
        );
        assert!(msg.contains("198.18.0.2"), "要点出实际值（现场证据）：{msg}");
        assert!(kept, "复检失败时快照必须留着（失败可重试语义）");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **判据 ①b（改前红）**：`0/1` 仍指向**本次会话的** utun ⇒ 必须 `Err`。
    ///
    /// 这是「整机没网」的真实入口：调用方随后会关掉 utun 的 fd，全机默认流量
    /// 就进了一条已死的隧道。而 `route -n get default` 仍答得出**系统的**默认路由
    /// ⇒ 只查默认路由**抓不到**这一条。
    #[test]
    fn rollback_refuses_to_claim_success_when_our_capture_route_is_still_on_the_tunnel() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;

        let root = tmp_snapshot_root("p0-capture-left");
        let mut snap = SessionSnapshot::new("s-cap".into(), "utun9".into(), fixture_uplink());
        // 延迟模式：捕获路由记在 pending 里（本次确实装过/尝试装过）。
        snap.pending_routes.push(InstalledRoute {
            destination: "0.0.0.0/1".parse().unwrap(),
            via: RouteVia::Interface {
                name: "utun9".into(),
            },
            replaced: None,
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        // 替身刻意让**默认路由是好的**（排除它干扰），只在 netstat 里保留一条
        // `0/1 → utun9` —— 也就是「删漏了自己的捕获路由」这一态。
        let exec: crate::macos::TestExecutor = std::rc::Rc::new(
            |program: &str, args: &[String]| {
                if program == crate::tools::ROUTE && argv_is(args, &["-n", "get", "default"]) {
                    return Ok(HEALTHY_DEFAULT_ROUTE.to_string());
                }
                if program == crate::tools::NETSTAT {
                    return Ok("0/1                utun9              UScg                utun9\n".to_string());
                }
                Ok(String::new())
            },
        );
        let (result, kept) = with_test_root(&root, || {
            let r = with_executor(exec, || rollback(&snap));
            (r.map(|_| ()), SessionSnapshot::snapshot_path().exists())
        });

        let msg = result
            .expect_err("捕获路由还在我们自己的 utun 上 ⇒ 不许报成功")
            .to_string();
        assert!(
            msg.contains("0/1") && msg.contains("utun9"),
            "要点名是哪条捕获路由、挂在哪个 utun 上：{msg}"
        );
        assert!(kept, "复检失败时快照必须留着");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **正向对照（改前改后都绿）**：系统确实回到原样时，回滚必须成功并删快照。
    ///
    /// 同时把判据 ① 的**字面形式**钉住：回滚之后 `route::default_route()`
    /// 仍能拿到 en0 那条（gateway 存在）。
    #[test]
    fn rollback_succeeds_when_the_route_and_dns_are_really_restored() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;

        let root = tmp_snapshot_root("p0-positive");
        let mut snap = SessionSnapshot::new("s-ok".into(), "utun9".into(), fixture_uplink());
        snap.installed_routes = vec![fixture_route("203.0.113.0/24")];
        // 原值 = 「没有设置任何 DNS 服务器」（DHCP）。`networksetup` 的原话就在这里。
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec![],
            search_domains: vec![],
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        let (result, gone) = with_test_root(&root, || {
            let r = with_executor(
                dispatch(
                    HEALTHY_DEFAULT_ROUTE,
                    "There aren't any DNS Servers set on Wi-Fi.\n",
                ),
                || rollback(&snap),
            );
            (r.map(|_| ()), !SessionSnapshot::snapshot_path().exists())
        });
        assert!(result.is_ok(), "系统已还原 ⇒ 回滚必须成功：{result:?}");
        assert!(gone, "全部复检通过才允许删快照");

        // 判据 ① 的字面形式。
        let dr = with_test_root(&root, || {
            with_executor(dispatch(HEALTHY_DEFAULT_ROUTE, ""), route::default_route)
        })
        .expect("回滚之后必须仍有一条可用的默认路由");
        assert_eq!(dr.interface, "en0");
        assert_eq!(dr.gateway, Some("192.168.0.1".parse().unwrap()));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 正向对照：备份里**有** DNS 时，「当前值 == 备份值」也算还原成功。
    #[test]
    fn rollback_accepts_a_restored_non_empty_dns_backup() {
        use crate::macos::snapshot::with_test_root;
        use crate::macos::with_executor;

        let root = tmp_snapshot_root("p0-dns-nonempty");
        let mut snap = SessionSnapshot::new("s-dns2".into(), "utun9".into(), fixture_uplink());
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec!["1.1.1.1".into(), "8.8.8.8".into()],
            search_domains: vec![],
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        let (result, gone) = with_test_root(&root, || {
            let r = with_executor(
                dispatch(HEALTHY_DEFAULT_ROUTE, "1.1.1.1\n8.8.8.8\n"),
                || rollback(&snap),
            );
            (r.map(|_| ()), !SessionSnapshot::snapshot_path().exists())
        });
        assert!(result.is_ok(), "DNS 已写回原值 ⇒ 必须成功：{result:?}");
        assert!(gone);
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------------
    // P0（本卡）：**启动对账** —— 判据从「标志位」改成「会话是否真的还活着」
    //
    // 现场：helper 重启/被重装而 App 不在时，快照仍写着 `Up`，但那个 utun
    // （fd 随 App 退出关闭）**已经不存在**；旧 `restore_stale()` 只看
    // `is_stale()`（`Up` ⇒ false）就跳过路由/DNS 清理，把清理交给
    // `force_cleanup` —— 而那一刻没人调它。于是系统 DNS 永久停在哨兵
    // `198.18.0.2`，用户「终端不通，浏览器有时候通」。
    //
    // 三条判据：
    //   ① 快照 `Up` + 接口不存在 ⇒ 启动路径必须恢复 DNS（改前红）；
    //   ①b 没有快照也要对账（重装/卸载把快照删了）⇒ 必须恢复 DNS（改前红）；
    //   ③ 正向对照：接口仍在（真有活会话）⇒ 一条 DNS 命令都不许发。
    // -----------------------------------------------------------------------

    /// 启动对账测试替身：`ifconfig -l` 由参数决定，记录所有 `-setdnsservers`。
    struct ResidueStub {
        exec: crate::macos::TestExecutor,
        set_calls: std::rc::Rc<std::cell::RefCell<Vec<Vec<String>>>>,
        cleared: std::rc::Rc<std::cell::Cell<bool>>,
    }

    impl ResidueStub {
        /// 在替身下跑 `f`（通常是 `restore_stale`），返回
        /// `(结果, 是否已清成 DHCP, 所有 -setdnsservers 调用)`。
        fn run<R>(self, f: impl FnOnce() -> R) -> (R, bool, Vec<Vec<String>>) {
            let cleared = std::rc::Rc::clone(&self.cleared);
            let calls = std::rc::Rc::clone(&self.set_calls);
            let r = crate::macos::with_executor(self.exec, f);
            let recorded = calls.borrow().clone();
            (r, cleared.get(), recorded)
        }
    }

    fn residue_stub(live_interfaces: &'static str) -> ResidueStub {
        use std::cell::{Cell, RefCell};
        use std::rc::Rc;

        let set_calls: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(Vec::new()));
        let cleared = Rc::new(Cell::new(false));
        let sink = Rc::clone(&set_calls);
        let flag = Rc::clone(&cleared);
        let exec: crate::macos::TestExecutor = Rc::new(move |program: &str, args: &[String]| {
            if program == crate::tools::IFCONFIG && argv_is(args, &["-l"]) {
                return Ok(live_interfaces.to_string());
            }
            if program == crate::tools::NETWORKSETUP {
                match args.first().map(String::as_str) {
                    Some("-listallnetworkservices") => {
                        return Ok(
                            "An asterisk (*) denotes that a network service is disabled.\nWi-Fi\n"
                                .to_string(),
                        )
                    }
                    Some("-setdnsservers") => {
                        sink.borrow_mut().push(args.to_vec());
                        if args.get(2).map(String::as_str) == Some("Empty") {
                            flag.set(true);
                        }
                        return Ok(String::new());
                    }
                    Some("-getdnsservers") => {
                        // 现场：Wi-Fi 的 DNS 就是哨兵；被清成 Empty 之后改答 DHCP 原话。
                        return Ok(if flag.get() {
                            "There aren't any DNS Servers set on Wi-Fi.\n".to_string()
                        } else {
                            "198.18.0.2\n".to_string()
                        });
                    }
                    _ => {}
                }
            }
            Ok(String::new())
        });
        ResidueStub { exec, set_calls, cleared }
    }

    /// **判据 ①（改前红）**：快照写着 `Up`、但它记的 utun 已不存在 ⇒ 必须恢复 DNS。
    ///
    /// 改前：`!is_stale()` ⇒ 直接 `Ok(None)`，一条 `networksetup` 都不发 ⇒ 红。
    #[test]
    fn startup_recovery_clears_sentinel_dns_when_the_recorded_utun_is_gone() {
        use crate::macos::snapshot::with_test_root;

        let root = tmp_snapshot_root("dns-residue-dead");
        let mut snap = SessionSnapshot::new("s-residue".into(), "utun9".into(), fixture_uplink());
        snap.state = SessionState::Up;
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec![], // 用户原值 = 未设置（DHCP）
            search_domains: vec![],
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        // 现场：`ifconfig -l` 里没有 utun9（App 不在 ⇒ fd 关闭 ⇒ 接口消失）。
        let stub = residue_stub("lo0 gif0 en0\n");
        let (result, cleared_now, calls) =
            with_test_root(&root, || stub.run(|| restore_stale().map(|o| o.is_some())));

        assert!(
            cleared_now,
            "接口已不在 ⇒ 启动路径必须把哨兵 DNS 清回 DHCP: {calls:?}"
        );
        assert_eq!(result.as_ref().ok(), Some(&true), "死会话应当被回滚掉: {result:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **判据 ①b（改前红）**：快照被卸载/重装删掉，DNS 仍停在哨兵上 ⇒
    /// 没有快照也要对账，否则用户永远修不回来。
    #[test]
    fn startup_recovery_clears_sentinel_dns_even_without_a_snapshot() {
        use crate::macos::snapshot::with_test_root;

        let root = tmp_snapshot_root("dns-residue-no-snap");
        let stub = residue_stub("lo0 en0\n");
        let (result, cleared_now, calls) =
            with_test_root(&root, || stub.run(|| restore_stale().map(|o| o.is_some())));

        assert!(
            cleared_now,
            "没有快照也要对账：哨兵 DNS 是整机解析失败的根因: {calls:?}"
        );
        assert_eq!(result.as_ref().ok(), Some(&false), "没有会话可回滚: {result:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **判据 ③ 正向对照（防过度清理）**：接口仍在 ⇒ 真有活会话 ⇒
    /// 一条 `-setdnsservers` 都不许发。
    ///
    /// 改前改后都绿；但它钉住了「不许因为发现哨兵就无脑清 DNS」——
    /// 一旦丢掉「会话是否活着」这一层判据，它立刻红。
    #[test]
    fn startup_recovery_leaves_a_live_sessions_sentinel_dns_alone() {
        use crate::macos::snapshot::with_test_root;

        let root = tmp_snapshot_root("dns-residue-live");
        let mut snap = SessionSnapshot::new("s-live".into(), "utun9".into(), fixture_uplink());
        snap.state = SessionState::Up;
        snap.dns_backups.push(crate::macos::dns::DnsBackup {
            service: "Wi-Fi".into(),
            servers: vec![],
            search_domains: vec![],
        });
        with_test_root(&root, || snap.save()).expect("写快照");

        // utun9 出现在接口表里 ⇒ 会话还活着（App 正持有 fd）。
        let stub = residue_stub("lo0 gif0 en0 utun9\n");
        let (result, calls) = with_test_root(&root, || {
            let (r, _cleared, calls) = stub.run(|| restore_stale().map(|o| o.is_some()));
            (r, calls)
        });

        assert!(
            calls.is_empty(),
            "有活会话时 DNS 指向哨兵是正常状态，绝不许动它: {calls:?}"
        );
        assert_eq!(result.as_ref().ok(), Some(&false), "活会话不在启动时拆: {result:?}");
        // 快照必须原样留着（下次启动/force_cleanup 还要用它）。
        let kept = with_test_root(&root, SessionSnapshot::load)
            .expect("读快照")
            .expect("活会话的快照不许被删");
        assert_eq!(kept.session_id, "s-live");
        let _ = std::fs::remove_dir_all(&root);
    }
}
