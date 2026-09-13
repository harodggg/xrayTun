//! helper 的服务端：Unix socket 监听 + 请求分发。
//!
//! # 威胁模型（决定了这里的每一行代码）
//!
//! helper 以 root 运行，socket 是提权通道。假想的攻击者是**同机上的
//! 非特权进程**（例如用户不小心运行的恶意脚本）。因此：
//!
//! * socket 权限 `root:admin 0660` —— 非 admin 组用户根本连不上（内核拦截）；
//! * 每条连接都做一次代码签名校验（audit token + `SecRequirement`）——
//!   即使是 admin 组的进程，只要不是我们签名的 App 也会被拒；
//! * **helper 不认识「代理」这个概念**。它只接受「建 utun / 加这条路由 /
//!   把这些 IP 设为 DNS」这类有限指令，且每条都经过 `xt_tun::validate` 校验。
//!   即使 GUI 被完全攻破，攻击者能做的也只是「改本机网络配置」，
//!   而不是「以 root 执行任意代码」。
//! * 所有外部命令都用绝对路径 + argv 数组调用，永不经过 shell。
//! * 可执行文件的路径必须落在白名单目录内（`/Library/PrivilegedHelperTools`、
//!   app bundle），否则等于把 root 执行权交出去。

use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use xt_proto::{
    DatapathPlan, DatapathStats, ErrorCode, HelperError, HelperStatus, HelloInfo, Request, Response,
    SessionStatus, TunFdInfo, TunUpRequest, HELPER_LABEL, PROTOCOL_VERSION,
};
use xt_tun::macos::controller::{self, BringUpOutcome};
use xt_tun::macos::{netif, snapshot::SessionSnapshot};
use xt_tun::plan::TunPlan;
use xt_tun::validate::validate_executable_path;

use crate::error::{internal, tun_err, Result};
use crate::peer::{self, PeerPolicy};
use crate::protocol;

/// 允许 helper 执行的数据面程序所在目录。
///
/// **这是 root 权限的执行白名单**，任何时候都不要放宽到 `/tmp`、用户家目录
/// 或 `/Applications` 下的任意位置。
const ALLOWED_EXEC_ROOTS: &[&str] = &[
    "/Library/PrivilegedHelperTools",
    "/Library/Application Support/XrayTun",
];

/// 子进程里 utun fd 的目标编号。
///
/// 之所以固定成 3，是因为要用 `XRAY_TUN_FD` 环境变量告诉子进程「fd 在哪」，
/// 而环境变量没法表达「继承 fd 的编号是动态的」这件事 —— 只有固定编号才可传递。
const CHILD_TUN_FD: RawFd = 3;

struct ActiveSession {
    snapshot: SessionSnapshot,
    /// 网络变更计划的**内存副本**。
    ///
    /// 它不落盘（含非序列化状态），但 `CommitRoutes` 需要它来装默认路由 + 切 DNS。
    /// 这也是「两阶段启动」的代价：helper 崩溃后 pending 路由只能被删除、
    /// 无法被提交 —— 这是刻意的，回滚永远比继续推进更安全。
    plan: TunPlan,
    /// helper 侧持有的 utun fd。
    ///
    /// **绝不能提前关闭**：utun 接口的生命周期绑定在 fd 上，fd 全部关闭
    /// 接口就消失。回滚时按快照删路由/dns，然后随进程一起释放。
    tun_fd: Option<RawFd>,
    datapath: Option<Child>,
}

struct State {
    session: Option<ActiveSession>,
}

pub struct Helper {
    state: Mutex<State>,
    policy: PeerPolicy,
    started_at: u64,
    socket_path: PathBuf,
}

impl Helper {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            state: Mutex::new(State { session: None }),
            policy: PeerPolicy::from_build_env(),
            started_at: now_unix(),
            socket_path,
        }
    }

    /// 启动前先回滚上次遗留的会话。
    ///
    /// 这一步保证了「helper 被 kill -9 之后重启」不会让用户永久处于断网状态。
    pub fn recover_from_crash(&self) {
        match controller::restore_stale() {
            Ok(Some(snap)) => tracing::warn!(
                session = %snap.session_id,
                interface = %snap.interface,
                "已回滚上次崩溃遗留的 TUN 会话"
            ),
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "回滚遗留会话失败（网络可能仍处于异常状态）"),
        }
    }

    /// 阻塞式服务循环。每个连接一个线程。
    ///
    /// 返回前一定会删掉 socket 文件。这一步不能省：
    ///
    /// 残留的 socket 文件会让 GUI 看到 `ECONNREFUSED` —— 文件在、但没人监听 ——
    /// 而这是**最难解释的一种状态**：看起来「已安装」，连接却直接被拒。
    /// 客户端如果把它当成「未安装」或「未授权」，用户就会去做完全无关的操作。
    pub fn serve(&self) -> Result<()> {
        let listener = bind_socket(&self.socket_path)?;
        tracing::info!(path = %self.socket_path.display(), "helper 已就绪");

        let result = self.accept_loop(&listener);

        // 无论怎么退出，都把 socket 摘干净。
        if let Err(e) = std::fs::remove_file(&self.socket_path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(error = %e, "清理 socket 文件失败");
            }
        }
        result
    }

    fn accept_loop(&self, listener: &std::os::unix::net::UnixListener) -> Result<()> {
        loop {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    // 单条连接一个线程：消息频率极低，线程模型最简单也最不容易出错；
                    // 而且「一问一答、不并发」正是 fd 传递时序正确性的前提。
                    //
                    // SAFETY: `self` 的生命周期覆盖整个 `serve()`，而这个循环
                    // 永不返回，所以把引用延长到 `'static` 让线程持有是安全的。
                    let helper: &'static Helper =
                        unsafe { std::mem::transmute::<&Helper, &'static Helper>(self) };
                    std::thread::spawn(move || {
                        if let Err(e) = handle_connection(helper, stream) {
                            tracing::warn!(error = %e, "连接处理结束（带错误）");
                        }
                    });
                }
                Err(e) => tracing::warn!(error = %e, "accept 失败"),
            }
        }
    }
}

/// 建立监听 socket 并收紧权限。
fn bind_socket(path: &Path) -> Result<std::os::unix::net::UnixListener> {
    // 上一次非正常退出可能留下残骸。先删掉，否则 bind 会 EADDRINUSE。
    match std::fs::remove_file(path) {
        Ok(()) => tracing::info!(path = %path.display(), "清理了残留的 socket 文件"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(internal(format!("删除残留 socket 失败: {e}"))),
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| internal(format!("创建 socket 目录失败: {e}")))?;
    }

    let listener = protocol::bind(path)
        .map_err(|e| internal(format!("bind {} 失败: {e}", path.display())))?;

    restrict_socket_permissions(path)?;
    Ok(listener)
}

/// `root:admin 0660`。
///
/// 注意：**只依赖权限位是不够的**，签名校验才是真正的门（见模块文档）。
/// 权限位的作用是把「非 admin 组用户」挡在最外层，减少无谓的校验开销
/// 与日志噪音。
fn restrict_socket_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .map_err(|e| internal(format!("设置 socket 权限失败: {e}")))?;

    // macOS 上 admin 组的 gid 是 80。用 getgrnam 查而不是写死，
    // 以防某些系统配置不同。
    let gid = admin_gid().unwrap_or(80);
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| internal("socket 路径含 NUL 字节"))?;
    // SAFETY: c_path 是有效的 C 字符串；chown 只读它。
    let rc = unsafe { libc::chown(c_path.as_ptr(), 0, gid) };
    if rc != 0 {
        return Err(internal(format!("chown socket 失败: {}", std::io::Error::last_os_error())));
    }
    Ok(())
}

fn admin_gid() -> Option<libc::gid_t> {
    let name = std::ffi::CString::new("admin").ok()?;
    // SAFETY: getgrnam 返回指向静态缓冲区的指针，我们在同一线程内立即读取 gid。
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        None
    } else {
        // SAFETY: gr 非空且由 getgrnam 保证至少在本线程内有效。
        Some(unsafe { (*gr).gr_gid })
    }
}

fn handle_connection(helper: &'static Helper, stream: UnixStream) -> Result<()> {
    // ---- 第一道：身份校验 ----
    let identity = peer::authorize(stream.as_raw_fd(), &helper.policy)?;
    tracing::info!(uid = identity.uid, pid = identity.pid, "已接受的连接");

    let mut handshaken = false;

    // 对端关闭或发来垃圾时 recv 报错，循环随之结束。
    while let Ok((request, incoming_fd)) = protocol::recv(&stream) {
        // 目前没有任何请求需要客户端附带 fd；收到就关掉，避免 fd 泄漏。
        if let Some(fd) = incoming_fd {
            // SAFETY: fd 由内核为我们新创建，所有权在此。
            unsafe { libc::close(fd) };
        }

        // 除 Hello 之外，必须先握手。
        if !handshaken && !matches!(request, Request::Hello { .. }) {
            let resp = Response::Error(HelperError::new(ErrorCode::NotHandshaken, "必须先发送 hello"));
            protocol::send(&stream, &resp)?;
            continue;
        }

        let (response, out_fd, exit_after) = helper.dispatch(request, &mut handshaken);

        // 协议规定的时序：先发帧，再发 fd。
        protocol::send(&stream, &response)?;
        if let Some(fd) = out_fd {
            protocol::send_fd(&stream, fd)?;
        }

        if exit_after {
            tracing::info!("按请求退出");
            std::process::exit(0);
        }
    }
    Ok(())
}

impl Helper {
    /// 返回 `(响应, 要附带的 fd, 处理完后是否退出进程)`。
    fn dispatch(
        &self,
        request: Request,
        handshaken: &mut bool,
    ) -> (Response, Option<RawFd>, bool) {
        match request {
            Request::Hello { client_version, protocol, client_name } => {
                if protocol != PROTOCOL_VERSION {
                    return (
                        Response::Error(HelperError::new(
                            ErrorCode::ProtocolMismatch,
                            format!("协议版本不匹配：helper={PROTOCOL_VERSION} 客户端={protocol}"),
                        )),
                        None,
                        false,
                    );
                }
                *handshaken = true;
                tracing::info!(client_version, client_name, "握手成功");

                let stale = SessionSnapshot::load()
                    .ok()
                    .flatten()
                    .filter(|s| s.is_stale())
                    .map(|s| s.session_id);
                let tun_active = self
                    .state
                    .lock()
                    .map(|s| s.session.is_some())
                    .unwrap_or(false);

                (
                    Response::Hello(HelloInfo {
                        helper_version: env!("CARGO_PKG_VERSION").to_string(),
                        protocol: PROTOCOL_VERSION,
                        binary_sha256: binary_sha256(),
                        tun_active,
                        stale_session: stale,
                    }),
                    None,
                    false,
                )
            }

            Request::Status => (Response::Status(Box::new(self.status())), None, false),

            Request::TunUp(req) => (self.tun_up(*req), None, false),

            Request::TakeTunFd { session_id } => self.take_tun_fd(&session_id),

            Request::CommitRoutes { session_id } => (self.commit_routes(&session_id), None, false),

            Request::TunDown { session_id } => (self.tun_down(&session_id), None, false),

            Request::Restore => {
                // 这里**必须**是「无论如何都清理」，而不是只清理陈旧会话。
                //
                // 曾经的写法是 `restore_stale()`，它带着 `is_stale()` 过滤：
                // 会话一旦提交完路由就变成 `Up`、`pending_routes` 也被清空，
                // 于是 `is_stale()` 为 false —— 一条**正在生效**的隧道会被
                // 判定成「不需要回滚」。后果是：
                //
                //   * 托盘「退出 XrayTun」→ 路由和 DNS 全部留在系统上；
                //   * 托盘「修复网络（回滚遗留配置）」→ 回复「没有需要回滚的会话」，
                //     用户点了修复却什么都没发生，而且此时他通常已经断网了。
                //
                // 退出与排障这两条路要的语义都是「把快照里记的改动全部撤销」，
                // 与「这条会话是不是崩在半路」无关。所以走 force_cleanup。
                //
                // 先拆内存里的活会话（顺手杀掉数据面进程、关掉 utun fd），
                // 再用磁盘快照兜底 —— 后者覆盖「helper 重启过、内存是空的」
                // 那种情况。两步都做才完整。
                let torn_down = self.tear_down_live_session();
                let cleaned = controller::force_cleanup();
                let resp = match cleaned {
                    Ok(Some(s)) => Response::Ok {
                        message: Some(format!("已回滚会话 {}", s.session_id)),
                    },
                    Ok(None) if torn_down => {
                        Response::Ok { message: Some("已回滚当前会话".into()) }
                    }
                    Ok(None) => Response::Ok { message: Some("没有需要回滚的会话".into()) },
                    Err(e) => Response::Error(tun_err(e)),
                };
                (resp, None, false)
            }

            Request::Stats { session_id } => (self.stats(&session_id), None, false),

            Request::Uninstall => (self.uninstall(), None, true),

            Request::Shutdown => (Response::Ok { message: Some("bye".into()) }, None, true),
        }
    }

    fn status(&self) -> HelperStatus {
        let sessions = match self.state.lock() {
            Ok(guard) => guard
                .session
                .as_ref()
                .map(|s| {
                    vec![SessionStatus {
                        session_id: s.snapshot.session_id.clone(),
                        interface: s.snapshot.interface.clone(),
                        mtu: 1500,
                        addresses: Vec::new(),
                        installed_routes: s.snapshot.installed_routes.clone(),
                        dns_modified: !s.snapshot.dns_backups.is_empty(),
                        datapath_pid: s.datapath.as_ref().map(|c| c.id()),
                        started_at_unix: s.snapshot.created_at,
                    }]
                })
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };

        HelperStatus {
            helper_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            started_at_unix: self.started_at,
            sessions,
            datapath_available: false,
            datapath_path: None,
        }
    }

    fn tun_up(&self, req: TunUpRequest) -> Response {
        // 已经有会话时先拒绝：并发的两个 TUN 会话会互相踩路由，
        // 而「谁该负责回滚」会变得无法判定。
        {
            let guard = match self.state.lock() {
                Ok(g) => g,
                Err(_) => return Response::Error(internal("状态锁中毒")),
            };
            if let Some(existing) = &guard.session {
                return Response::Error(HelperError::new(
                    ErrorCode::InvalidRequest,
                    format!(
                        "已有活跃会话 {}（接口 {}），请先 tun_down",
                        existing.snapshot.session_id, existing.snapshot.interface
                    ),
                ));
            }
        }

        // 数据面可执行文件路径必须在白名单内 —— 这是 root 执行的唯一入口。
        if let DatapathPlan::SpawnDatapath { binary, .. } = &req.datapath {
            let roots: Vec<&Path> = ALLOWED_EXEC_ROOTS.iter().map(Path::new).collect();
            if let Err(e) = validate_executable_path(Path::new(binary), &roots) {
                return Response::Error(HelperError::new(ErrorCode::Unauthorized, e.to_string()));
            }
        }

        let outcome: BringUpOutcome = match controller::bring_up(&req) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(error = %e, "建立 TUN 失败");
                return Response::Error(tun_err(e));
            }
        };

        let spawned = match &req.datapath {
            DatapathPlan::SpawnDatapath { binary, args, use_helper_fd } => {
                let tun_fd = if *use_helper_fd {
                    match outcome.handed_fd {
                        Some(fd) => fd,
                        None => {
                            let _ = controller::rollback(&outcome.snapshot);
                            return Response::Error(HelperError::new(
                                ErrorCode::DatapathFailed,
                                "请求了 fd 传递，但 helper 没有可交付的 utun fd",
                            ));
                        }
                    }
                } else {
                    // 子进程自建 utun：不能给它我们的 fd（那会让它跳过自己的
                    // 建卡/配地址逻辑，反而是我们没配置过的状态）。
                    -1
                };
                match spawn_datapath(binary, args, tun_fd) {
                    Ok(child) => Some(child),
                    Err(e) => {
                        // 数据面起不来，整个会话就没有意义：立刻回滚。
                        tracing::error!(error = %e, "数据面启动失败，回滚会话");
                        let _ = controller::rollback(&outcome.snapshot);
                        return Response::Error(HelperError::new(ErrorCode::DatapathFailed, e.to_string()));
                    }
                }
            }
            DatapathPlan::HandoffFd => None,
        };

        let mut snapshot = outcome.snapshot;
        if let Some(child) = &spawned {
            snapshot.datapath_pid = Some(child.id());
            let _ = snapshot.save();
        }

        let deferred = req.defer_default_routes;
        let message = if deferred {
            format!(
                "接口 {}（{} 条 bypass 路由）已就绪，等待 CommitRoutes 接管默认路由",
                snapshot.interface,
                snapshot.installed_routes.len()
            )
        } else {
            format!(
                "接口 {}（{} 条路由{}）已就绪",
                snapshot.interface,
                snapshot.installed_routes.len(),
                if snapshot.dns_backups.is_empty() { "" } else { "，DNS 已切换" }
            )
        };

        let session = ActiveSession {
            snapshot,
            plan: outcome.plan,
            tun_fd: outcome.handed_fd,
            datapath: spawned,
        };
        match self.state.lock() {
            Ok(mut guard) => guard.session = Some(session),
            Err(_) => return Response::Error(internal("状态锁中毒")),
        }

        tracing::info!(%message, deferred, "TUN 会话已建立");
        Response::Ok { message: Some(message) }
    }

    /// 两阶段启动的第二步。
    fn commit_routes(&self, session_id: &str) -> Response {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => return Response::Error(internal("状态锁中毒")),
        };
        let Some(session) = guard.session.as_mut() else {
            return Response::Error(HelperError::new(ErrorCode::NoSuchSession, "没有活跃会话"));
        };
        if session.snapshot.session_id != session_id {
            return Response::Error(HelperError::new(ErrorCode::NoSuchSession, "会话 id 不匹配"));
        }
        if session.snapshot.pending_routes.is_empty() {
            // 幂等：重复提交不算错误，GUI 重试时不该看到失败。
            return Response::Ok { message: Some("没有待提交的路由（可能已经提交过）".into()) };
        }

        match controller::commit_routes_and_dns(&mut session.snapshot, &session.plan) {
            Ok(()) => {
                let n = session.snapshot.installed_routes.len();
                tracing::info!(session_id, routes = n, "默认路由已接管，DNS 已切换");
                Response::Ok { message: Some(format!("已接管默认路由（共 {n} 条）")) }
            }
            Err(e) => {
                // 提交失败：立刻整体回滚，绝不能让用户停在「路由改了一半」的状态。
                tracing::error!(error = %e, "提交路由失败，回滚会话");
                let snap = session.snapshot.clone();
                let _ = guard.session.take();
                let _ = controller::rollback(&snap);
                Response::Error(tun_err(e))
            }
        }
    }

    fn take_tun_fd(&self, session_id: &str) -> (Response, Option<RawFd>, bool) {
        let guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => return (Response::Error(internal("状态锁中毒")), None, false),
        };
        let Some(session) = &guard.session else {
            return (
                Response::Error(HelperError::new(ErrorCode::NoSuchSession, "没有活跃会话")),
                None,
                false,
            );
        };
        if session.snapshot.session_id != session_id {
            return (
                Response::Error(HelperError::new(
                    ErrorCode::NoSuchSession,
                    format!("会话 id 不匹配：当前 {}", session.snapshot.session_id),
                )),
                None,
                false,
            );
        }
        let Some(fd) = session.tun_fd else {
            return (
                Response::Error(HelperError::new(
                    ErrorCode::InvalidRequest,
                    "该会话不是 handoff_fd 模式，没有可交付的 fd",
                )),
                None,
                false,
            );
        };

        (
            Response::TunFd(TunFdInfo {
                session_id: session.snapshot.session_id.clone(),
                interface: session.snapshot.interface.clone(),
                mtu: 1500,
                // macOS 的 utun 每个包前有 4 字节地址族头。
                header_len: 4,
            }),
            // 把 fd 交给内核去 dup：sendmsg(SCM_RIGHTS) 会在接收方新建一个描述符，
            // 我们这边的 fd 仍然由会话持有，保证接口不会因为交付而消失。
            Some(fd),
            false,
        )
    }

    fn tun_down(&self, session_id: &str) -> Response {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => return Response::Error(internal("状态锁中毒")),
        };

        let Some(mut session) = guard.session.take() else {
            return Response::Error(HelperError::new(ErrorCode::NoSuchSession, "没有活跃会话"));
        };

        // 陈旧请求不能误拆后来建立的新会话。
        if session.snapshot.session_id != session_id {
            let current = session.snapshot.session_id.clone();
            guard.session = Some(session);
            return Response::Error(HelperError::new(
                ErrorCode::NoSuchSession,
                format!("会话 id 不匹配（请求 {session_id}，当前 {current}）"),
            ));
        }

        // 先杀数据面：它还持有 utun fd，不先停掉的话接口不会消失，
        // 路由删除也可能被内核拒绝。
        if let Some(mut child) = session.datapath.take() {
            let _ = child.kill();
            let _ = child.wait();
        }

        let result = controller::rollback(&session.snapshot);
        if let Some(fd) = session.tun_fd.take() {
            // SAFETY: fd 由本会话独占，这里关闭它让 utun 接口消失。
            unsafe { libc::close(fd) };
        }

        match result {
            Ok(()) => Response::Ok { message: Some("已回滚路由与 DNS".into()) },
            Err(e) => Response::Error(tun_err(e)),
        }
    }

    /// 拆掉内存里那条活会话：杀数据面、回滚路由与 DNS、关掉 utun fd。
    ///
    /// 返回是否真的拆了一条。用于 `Restore` —— 见那里的注释。
    /// 与 `tun_down` 的区别是它不校验 session_id：排障时我们就是要
    /// 「不管现在挂着什么，都给我拆掉」。
    fn tear_down_live_session(&self) -> bool {
        let Ok(mut guard) = self.state.lock() else {
            return false;
        };
        let Some(mut session) = guard.session.take() else {
            return false;
        };

        // 顺序与 tun_down 一致：数据面还持有 utun fd，不先停掉的话
        // 接口不会消失，路由删除也可能被内核拒绝。
        if let Some(mut child) = session.datapath.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Err(e) = controller::rollback(&session.snapshot) {
            tracing::error!(error = %e, "强制清理时回滚路由失败");
        }
        if let Some(fd) = session.tun_fd.take() {
            // SAFETY: fd 由本会话独占，这里关闭它让 utun 接口消失。
            unsafe { libc::close(fd) };
        }
        true
    }

    fn stats(&self, session_id: &str) -> Response {
        let guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => return Response::Error(internal("状态锁中毒")),
        };
        let Some(session) = &guard.session else {
            return Response::Error(HelperError::new(ErrorCode::NoSuchSession, "没有活跃会话"));
        };
        if session.snapshot.session_id != session_id {
            return Response::Error(HelperError::new(ErrorCode::NoSuchSession, "会话 id 不匹配"));
        }

        let counters = netif::interface_counters(&session.snapshot.interface).unwrap_or_default();
        Response::Stats(DatapathStats {
            session_id: session.snapshot.session_id.clone(),
            uptime_secs: now_unix().saturating_sub(session.snapshot.created_at),
            rx_bytes: counters.rx_bytes,
            tx_bytes: counters.tx_bytes,
            active_connections: None,
        })
    }

    fn uninstall(&self) -> Response {
        // 先回滚，再摘 launchd，最后删文件。顺序错了会留下「服务已卸载但
        // 路由还在」的状态。
        if let Ok(mut guard) = self.state.lock() {
            if let Some(session) = guard.session.take() {
                let _ = controller::rollback(&session.snapshot);
            }
        }
        let _ = controller::force_cleanup();

        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("system/{HELPER_LABEL}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let _ = std::fs::remove_file(xt_proto::HELPER_PLIST_PATH);
        let _ = std::fs::remove_file(&self.socket_path);
        // 最后删自己。删掉之后进程仍在运行（inode 还在），由调用方要求退出。
        let _ = std::fs::remove_file(xt_proto::HELPER_INSTALLED_PATH);

        Response::Ok { message: Some("helper 已卸载".into()) }
    }
}

/// 以 root 拉起数据面子进程。
///
/// `tun_fd >= 0` 时把 utun fd 以固定编号 `CHILD_TUN_FD` 传下去，
/// 并通过 `XRAY_TUN_FD` 告诉子进程；`tun_fd < 0` 时子进程自己建卡。
fn spawn_datapath(binary: &str, args: &[String], tun_fd: RawFd) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;

    let mut cmd = Command::new(binary);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());

    if tun_fd >= 0 {
        // Xray 同时接受 `xray.tun.fd` 与 `XRAY_TUN_FD`，两个都设上更保险。
        cmd.env("XRAY_TUN_FD", CHILD_TUN_FD.to_string())
            .env("xray.tun.fd", CHILD_TUN_FD.to_string())
            .env("XRAYTUN_TUN_FD", CHILD_TUN_FD.to_string());

        // SAFETY: pre_exec 在 fork 之后、exec 之前运行，只能调用
        // async-signal-safe 的函数。dup2 满足要求，且这里不做任何分配。
        unsafe {
            cmd.pre_exec(move || {
                if libc::dup2(tun_fd, CHILD_TUN_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = cmd.spawn()?;
    tracing::info!(binary, pid = child.id(), tun_fd, "数据面已启动");
    Ok(child)
}

/// helper 自身二进制的 SHA-256，用于让 GUI 检测版本漂移。
///
/// 用 `shasum` 而不是引入 `sha2` 依赖：helper 的依赖越少越好，
/// 而这个值只在握手时算一次。
fn binary_sha256() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return String::new();
    };
    Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(&exe)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(|x| x.to_string()))
        .unwrap_or_default()
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// 让 Windows 之外的平台也知道这个常量存在（避免 dead_code 警告）。
#[allow(dead_code)]
pub const SPAWN_GRACE: Duration = Duration::from_secs(2);

/// 仅测试使用：判断某条错误是否表示「已经有一个会话」。
#[cfg(test)]
pub fn is_conflict(e: &HelperError) -> bool {
    e.code == ErrorCode::InvalidRequest && e.message.contains("已有活跃会话")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::invalid;

    #[test]
    fn exec_whitelist_does_not_include_writable_user_dirs() {
        // 这是安全不变式：白名单里绝不能出现 /tmp、/Users 等可写目录。
        for root in ALLOWED_EXEC_ROOTS {
            assert!(!root.starts_with("/tmp"), "{root} 不应在白名单里");
            assert!(!root.starts_with("/Users"), "{root} 不应在白名单里");
            assert!(!root.starts_with("/var/tmp"), "{root} 不应在白名单里");
        }
    }

    #[test]
    fn validate_rejects_binary_outside_whitelist() {
        let roots: Vec<&Path> = ALLOWED_EXEC_ROOTS.iter().map(Path::new).collect();
        assert!(validate_executable_path(Path::new("/tmp/evil"), &roots).is_err());
        assert!(
            validate_executable_path(Path::new("/Library/PrivilegedHelperTools/xray"), &roots).is_ok()
        );
    }

    #[test]
    fn child_fd_number_is_stable() {
        // 环境变量 XRAY_TUN_FD 只能表达固定编号，所以这个常量不能随便改。
        assert_eq!(CHILD_TUN_FD, 3);
    }

    #[test]
    fn conflict_detection_matches_its_message() {
        let e = HelperError::new(ErrorCode::InvalidRequest, "已有活跃会话 s-1（接口 utun4），请先 tun_down");
        assert!(is_conflict(&e));
        assert!(!is_conflict(&invalid("别的问题")));
    }

    #[test]
    fn admin_gid_lookup_does_not_panic() {
        // 在真实 macOS 上 admin 组一定存在；这里只要求不 panic。
        let _ = admin_gid();
    }
}
