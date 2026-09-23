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

use std::os::unix::io::RawFd;

use xt_proto::{DatapathPlan, DnsMode, InstalledRoute, RouteVia, TunUpRequest};

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
    let mut snap = SessionSnapshot::new(req.session_id.clone(), ifname.clone(), physical);
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

/// 按快照回滚。**尽力而为**：单项失败不影响其余项。
pub fn rollback(snap: &SessionSnapshot) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();

    // 1) DNS 先还原（见模块文档里的顺序说明）
    for backup in snap.dns_backups.iter().rev() {
        if let Err(e) = dns::restore(backup) {
            failures.push(format!("还原 {} 的 DNS 失败: {e}", backup.service));
        }
    }

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
    for action in rollback_plan(&all_routes) {
        match action {
            RollbackAction::Delete { destination, via } => {
                if let Err(e) = route::delete(&destination, &via) {
                    failures.push(format!("删除路由 {destination} 失败: {e}"));
                }
            }
            RollbackAction::Restore { destination, via } => {
                if let Err(e) = route::add(&destination, &via) {
                    failures.push(format!(
                        "恢复路由 {destination}（原本经由 {via:?}）失败: {e}"
                    ));
                }
            }
        }
    }

    // 3) 数据面进程由调用方（helper）负责杀掉，这里只报告 pid
    if let Some(pid) = snap.datapath_pid {
        tracing::info!(pid, "回滚：请调用方终止数据面进程");
    }

    // 4) 全部成功才删快照；有失败就留着让下次启动重试。
    if failures.is_empty() {
        SessionSnapshot::clear()?;
        Ok(())
    } else {
        Err(Error::Invalid(failures.join("; ")))
    }
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

/// 回滚上次遗留的会话（GUI 崩溃 / helper 被杀之后调用）。
pub fn restore_stale() -> Result<Option<SessionSnapshot>> {
    let Some(snap) = SessionSnapshot::load()? else {
        return Ok(None);
    };
    if !snap.is_stale() {
        return Ok(None);
    }
    tracing::warn!(
        session = %snap.session_id,
        interface = %snap.interface,
        routes = snap.installed_routes.len(),
        "发现未清理的 TUN 会话，正在回滚"
    );
    rollback(&snap)?;
    Ok(Some(snap))
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

        // 「失败可重试」也要行为级成立：换全成功的执行器再跑一次 ⇒ Ok，且这时才删快照。
        let ok: crate::macos::TestExecutor = Rc::new(|_, _| Ok(String::new()));
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
}
