//! macOS 编排层：把 utun/地址/路由/DNS/快照 串成两阶段流程。
//!
//! 两阶段（对应 §5 的 `tun_up` / `commit_routes`）：
//! 1. `tun_up`：建卡 + 配地址 + 装 bypass 路由 + 落快照（**不碰默认路由/DNS**）；
//! 2. `commit_routes`：装 default 路由 + 改 DNS，落成 `Up`。
//!
//! 断开走 `tun_down`，按快照倒序撤销；崩溃残留由 `restore_stale` 处理。

pub(crate) mod dns;
pub(crate) mod netif;
pub(crate) mod route;
pub(crate) mod snapshot;
pub(crate) mod utun;

use std::os::fd::RawFd;
use std::path::Path;
use std::sync::Mutex;

use xt_contract::error::{bad_request, conflict, internal, not_found, ErrorBody, ErrorCode};

use crate::model::{InstalledRoute, RouteVia, SessionState, Snapshot, TunRequest, TunSession};
use crate::validate;

/// 快照目录（helper 的 root 状态目录）。`restore_stale` 单独收一个 `state_dir`
/// 以便 helper 按自己的配置传；会话函数用这个固定默认值。
pub(crate) const DEFAULT_STATE_DIR: &str = "/Library/Application Support/XrayTun";

/// 外部命令绝对路径，永不依赖 `PATH`。
pub(crate) mod tools {
    pub const IFCONFIG: &str = "/sbin/ifconfig";
    pub const ROUTE: &str = "/sbin/route";
    pub const NETSTAT: &str = "/usr/sbin/netstat";
    pub const NETWORKSETUP: &str = "/usr/sbin/networksetup";
}

/// 真正执行外部命令。**只用绝对路径 + argv 数组，永不经过 shell**——
/// 这是 helper 安全性的基石：任何来自 GUI 的字符串都不可能被解释成 shell 语法。
///
/// 失败时把 `stderr` 塞进 `detail`，让 `route::delete` 能判断「路由本就不存在」。
pub(crate) fn run(program: &str, args: &[String]) -> Result<String, ErrorBody> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| ErrorBody::new(ErrorCode::Io, format!("执行 {program} 失败：{e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ErrorBody::new(
            ErrorCode::Io,
            format!("{program} 执行失败（退出码 {:?}）", output.status.code()),
        )
        .with_detail(serde_json::json!({
            "program": program,
            "exit_code": output.status.code(),
            "stderr": stderr,
        })));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(crate) fn run_ok(program: &str, args: &[String]) -> Result<(), ErrorBody> {
    run(program, args).map(|_| ())
}

pub(crate) fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// 当前活跃会话。helper 一次只服务一个 TUN 会话；fd 在 `tun_up` 与 `take_fd`
/// 之间由本进程持有。
struct ActiveSession {
    id: String,
    fd: Option<RawFd>,
}

static ACTIVE: Mutex<Option<ActiveSession>> = Mutex::new(None);

pub(crate) fn tun_up(req: &TunRequest) -> Result<TunSession, ErrorBody> {
    // 会话级排他：同一时刻只能有一张 TUN。
    {
        let guard = ACTIVE.lock().map_err(|_| internal("会话锁中毒"))?;
        if guard.is_some() {
            return Err(conflict("已存在活跃 TUN 会话"));
        }
    }

    let addresses = validate::parse_cidrs(&req.addresses)?;
    let bypass = validate::parse_cidrs(&req.bypass_routes)?;
    let defaults = validate::parse_cidrs(&req.default_routes)?;

    let physical = route::discover_physical()?;
    let gateway = physical
        .gateway
        .ok_or_else(|| bad_request("物理上行没有默认网关，无法装旁路路由"))?;

    let (fd, interface) = utun::create()?;

    // 建卡之后的任何失败都要拆卡（关 fd 即销毁 utun）。
    let built = (|| -> Result<Snapshot, ErrorBody> {
        for addr in &addresses {
            netif::configure_address(&interface, addr, req.mtu)?;
        }
        let mut installed = Vec::new();
        for dest in &bypass {
            let via = RouteVia::Gateway { addr: gateway };
            let replaced = route::existing_route(dest);
            route::add(dest, &via)?;
            installed.push(InstalledRoute { destination: *dest, via, replaced });
        }
        let pending: Vec<InstalledRoute> = defaults
            .iter()
            .map(|d| InstalledRoute {
                destination: *d,
                via: RouteVia::Interface { name: interface.clone() },
                replaced: None,
            })
            .collect();

        let snap = Snapshot {
            session_id: new_session_id(),
            interface: interface.clone(),
            state: SessionState::BringingUp,
            addresses,
            installed_routes: installed,
            pending_routes: pending,
            physical: Some(physical),
            dns_servers: req.dns_servers.clone(),
            dns: None,
        };
        snapshot::save(&snap, Path::new(DEFAULT_STATE_DIR))?;
        Ok(snap)
    })();

    match built {
        Ok(snap) => {
            let session = TunSession {
                id: snap.session_id.clone(),
                interface: snap.interface.clone(),
            };
            *ACTIVE.lock().map_err(|_| internal("会话锁中毒"))? = Some(ActiveSession {
                id: snap.session_id,
                fd: Some(fd),
            });
            Ok(session)
        }
        Err(e) => {
            // SAFETY: fd 是本进程刚创建的 utun，尚未交出。
            unsafe { libc::close(fd) };
            Err(e)
        }
    }
}

pub(crate) fn take_fd(session: &TunSession) -> Result<RawFd, ErrorBody> {
    let mut guard = ACTIVE.lock().map_err(|_| internal("会话锁中毒"))?;
    let active = guard.as_mut().ok_or_else(|| not_found("无活跃 TUN 会话"))?;
    if active.id != session.id {
        return Err(not_found("会话 id 不匹配"));
    }
    active.fd.take().ok_or_else(|| conflict("fd 已经交出"))
}

pub(crate) fn commit_routes(session: &TunSession) -> Result<(), ErrorBody> {
    {
        let guard = ACTIVE.lock().map_err(|_| internal("会话锁中毒"))?;
        let active = guard.as_ref().ok_or_else(|| not_found("无活跃 TUN 会话"))?;
        if active.id != session.id {
            return Err(not_found("会话 id 不匹配"));
        }
    }

    let mut snap = snapshot::load(Path::new(DEFAULT_STATE_DIR))?
        .ok_or_else(|| not_found("无会话快照"))?;

    // 装 pending 的 default 路由（走 utun 接口）。
    let pending = std::mem::take(&mut snap.pending_routes);
    for r in &pending {
        route::add(&r.destination, &r.via)?;
    }
    snap.installed_routes.extend(pending);

    // 改 DNS：先备份再改。
    let backup = dns::backup()?;
    dns::set(&snap.dns_servers)?;
    snap.dns = Some(backup);

    snap.state = SessionState::Up;
    snapshot::save(&snap, Path::new(DEFAULT_STATE_DIR))?;
    Ok(())
}

pub(crate) fn tun_down(session: &TunSession) -> Result<(), ErrorBody> {
    let mut guard = ACTIVE.lock().map_err(|_| internal("会话锁中毒"))?;
    let active = guard.as_mut().ok_or_else(|| not_found("无活跃 TUN 会话"))?;
    if active.id != session.id {
        return Err(not_found("会话 id 不匹配"));
    }

    let snap = snapshot::load(Path::new(DEFAULT_STATE_DIR))?
        .ok_or_else(|| not_found("无会话快照"))?;

    rollback(&snap)?;
    snapshot::clear(Path::new(DEFAULT_STATE_DIR))?;

    // 若 fd 还在手里（还没 take_fd），关闭以销毁接口。
    if let Some(fd) = active.fd.take() {
        // SAFETY: fd 由本进程持有。
        unsafe { libc::close(fd) };
    }
    *guard = None;
    Ok(())
}

pub(crate) fn restore_stale(state_dir: &Path) -> Result<Option<String>, ErrorBody> {
    let Some(snap) = snapshot::load(state_dir)? else {
        return Ok(None);
    };
    // 只回滚「崩在半路」的（BringingUp/TearingDown 或 pending 非空）。
    if !snap.is_stale() {
        return Ok(None);
    }
    let interface = snap.interface.clone();
    rollback(&snap)?;
    snapshot::clear(state_dir)?;
    Ok(Some(interface))
}

/// 按快照倒序撤销：DNS → 路由 → 接口（接口由关 fd 销毁，见调用方）。
fn rollback(snap: &Snapshot) -> Result<(), ErrorBody> {
    if let Some(backup) = &snap.dns {
        dns::restore(backup)?;
    }
    for r in snap.installed_routes.iter().rev() {
        route::delete(&r.destination, &r.via)?;
        if let Some(replaced) = &r.replaced {
            route::add(&r.destination, replaced)?;
        }
    }
    Ok(())
}

fn new_session_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("sess-{ts:x}-{n:x}")
}
