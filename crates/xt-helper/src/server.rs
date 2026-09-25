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
    SessionStatus, TrustStatus, TunFdInfo, TunUpRequest, HELPER_LABEL, PROTOCOL_VERSION,
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
        let policy = PeerPolicy::from_build_env();
        // 策略必须在**启动日志**里可见：排障时第一句要看的就是"这道门现在哪种"，
        // 产物断言脚本也用它给出的 `XRAYTUN_HELPER_POLICY=…` 标识。
        tracing::info!(policy = %policy.describe(), "对端授权策略");
        Self {
            state: Mutex::new(State { session: None }),
            policy,
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
            // 信任锚：MITM 用的本地根证书（安装/移除都要落在会话快照上）。
            Request::InstallTrustAnchor { pem, fingerprint } => {
                (self.install_trust_anchor(&pem, &fingerprint), None, false)
            }
            Request::RemoveTrustAnchor { fingerprint } => {
                (self.remove_trust_anchor(&fingerprint), None, false)
            }

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

    /// 安装信任锚（MITM 的本地根证书）。
    ///
    /// # 为什么**必须**有活跃 TUN 会话
    ///
    /// 信任锚要挂在**可回滚的快照**上：helper 被 kill -9 之后，下次启动第一件事就是
    /// 读快照回滚，而快照是按会话存的。没有会话就没有回滚依据 ⇒ 一个"装进系统钥匙串、
    /// 却没人记得它"的根证书会永久留下。所以这里**宁可拒绝，也不装**。
    fn install_trust_anchor(&self, pem: &str, fingerprint: &str) -> Response {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => {
                return Response::Error(HelperError::new(ErrorCode::Internal, "状态锁中毒"));
            }
        };
        let Some(session) = guard.session.as_mut() else {
            return Response::Error(HelperError::new(
                ErrorCode::InvalidRequest,
                "没有正在运行的 TUN 会话：信任锚必须挂在可回滚的会话快照上，请先建立隧道",
            ));
        };
        let backup = match xt_tun::macos::trust::install(pem, fingerprint) {
            Ok(b) => b,
            Err(e) => {
                return Response::Error(HelperError::new(
                    ErrorCode::NetworkConfigFailed,
                    format!("安装信任锚失败：{e}"),
                ));
            }
        };
        session.snapshot.trust_anchors.push(backup.clone());
        if let Err(e) = session.snapshot.save() {
            // **落盘失败必须把已经装上的撤掉。** 否则系统里多了一个"没人记得"的根证书：
            // helper 一重启，回滚逻辑根本不知道它存在 —— 这正是本模块最怕的状态。
            let undone = xt_tun::macos::trust::rollback(&backup);
            return Response::Error(HelperError::new(
                ErrorCode::Internal,
                format!("写快照失败（{e}）⇒ 已撤销刚装的信任锚（撤销结果 {undone:?}）"),
            ));
        }
        Response::Trust(TrustStatus {
            installed: true,
            fingerprint: backup.fingerprint.clone(),
            cert_path: Some(backup.cert_path.clone()),
            existed_before: backup.existed_before,
            note: if backup.existed_before {
                Some("这个指纹在安装前就已被信任 —— 回滚时不会删它".into())
            } else {
                None
            },
        })
    }

    /// 移除信任锚。**幂等**：不在快照里也照样尝试删，并**如实说明**"这是没记上的一次移除"。
    fn remove_trust_anchor(&self, fingerprint: &str) -> Response {
        let mut guard = match self.state.lock() {
            Ok(g) => g,
            Err(_) => {
                return Response::Error(HelperError::new(ErrorCode::Internal, "状态锁中毒"));
            }
        };
        let Some(session) = guard.session.as_mut() else {
            return Response::Error(HelperError::new(
                ErrorCode::InvalidRequest,
                "没有正在运行的 TUN 会话",
            ));
        };
        let normalized = xt_tun::macos::trust::normalize_fingerprint(fingerprint);
        let recorded = session
            .snapshot
            .trust_anchors
            .iter()
            .find(|b| b.fingerprint == normalized)
            .cloned();
        let note = match &recorded {
            Some(backup) => {
                if let Err(e) = xt_tun::macos::trust::rollback(backup) {
                    return Response::Error(HelperError::new(
                        ErrorCode::NetworkConfigFailed,
                        format!("移除信任锚失败：{e}"),
                    ));
                }
                None
            }
            None => {
                // 快照里没有 ⇒ 仍然尝试删（幂等），但**不许静默**：如实说这是没记上的一次。
                if let Err(e) = xt_tun::macos::trust::remove(&normalized) {
                    return Response::Error(HelperError::new(
                        ErrorCode::NetworkConfigFailed,
                        format!("移除信任锚失败：{e}"),
                    ));
                }
                Some("这个指纹不在会话快照里（可能不是本会话装的）—— 仍然尝试删了".to_string())
            }
        };
        session
            .snapshot
            .trust_anchors
            .retain(|b| b.fingerprint != normalized);
        if let Err(e) = session.snapshot.save() {
            return Response::Error(HelperError::new(
                ErrorCode::Internal,
                format!("写快照失败：{e}"),
            ));
        }
        Response::Trust(TrustStatus {
            installed: false,
            fingerprint: normalized,
            cert_path: None,
            existed_before: recorded.map(|b| b.existed_before).unwrap_or(false),
            note,
        })
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
                // **码要与「路径白名单失败」那类 InvalidRequest 分开**：
                // 桌面端只对 `SessionConflict` 触发「自动清理 + 重试一次」，
                // 其余错误必须原样暴露给用户（见 xt_proto::HelperError
                // ::is_session_conflict 的注释）。文案保留中文，便于人读。
                return Response::Error(HelperError::new(
                    ErrorCode::SessionConflict,
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
                            // 回滚失败也**要说出来**（task-122 A-2）：主错误已经返回了，
                            // 但用户很可能据此以为「网络已经清干净」——所以把回滚的
                            // 结果一并带上，而不是 `let _ =` 吞掉。
                            let retry = match controller::rollback(&outcome.snapshot) {
                                Ok(()) => String::new(),
                                Err(e) => {
                                    tracing::warn!(error = %e, "回滚网络配置失败（fd 传递缺失）");
                                    format!("；回滚网络配置也失败：{e}")
                                }
                            };
                            return Response::Error(HelperError::new(
                                ErrorCode::DatapathFailed,
                                format!("请求了 fd 传递，但 helper 没有可交付的 utun fd{retry}"),
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
                        let retry = match controller::rollback(&outcome.snapshot) {
                            Ok(()) => String::new(),
                            Err(re) => {
                                tracing::warn!(error = %re, "回滚网络配置失败（数据面启动失败之后）");
                                format!("；回滚网络配置也失败：{re}")
                            }
                        };
                        return Response::Error(HelperError::new(
                            ErrorCode::DatapathFailed,
                            format!("{e}{retry}"),
                        ));
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
                // 回滚失败**至少留痕**（task-122 A-2）：主错误（提交失败）已经返回，
                // 但「回滚没成功」意味着路由可能处于半改状态，日志里必须查得到。
                if let Err(re) = controller::rollback(&snap) {
                    tracing::warn!(
                        error = %re,
                        "提交失败后的回滚也失败 —— 路由/DNS 可能停在半改状态"
                    );
                }
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
        // 站点的**唯一出口**，而且是**一行委托**：取两处回滚结果 → 纯函数给出（原因, 响应）
        // → 记日志 → 清理落盘痕迹。守卫会要求这一层**只有这一行委托**（任何「自己造响应」
        // 的写法都判假）；响应文案的取舍全在 `uninstall_outcome`（有行为测试）。
        self.uninstall_with(
            || self.rollback_memory_session(),
            || {
                controller::force_cleanup()
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            },
            |helper| helper.remove_system_traces(),
        )
    }

    /// 取「内存里那份会话」的回滚结果：没有会话 / 锁中毒 ⇒ `Ok`（与旧实现一致）。
    fn rollback_memory_session(&self) -> Result<(), String> {
        match self.state.lock() {
            Ok(mut guard) => guard
                .session
                .take()
                .map(|session| {
                    controller::rollback(&session.snapshot).map_err(|e| e.to_string())
                })
                .unwrap_or(Ok(())),
            Err(_) => Ok(()),
        }
    }

    /// 摘 launchd + 删落盘痕迹。**与响应无关**的副作用，单独一层便于注入/跳过。
    fn remove_system_traces(&self) {
        let _ = Command::new("/bin/launchctl")
            .args(["bootout", &format!("system/{HELPER_LABEL}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let _ = std::fs::remove_file(xt_proto::HELPER_PLIST_PATH);
        let _ = std::fs::remove_file(&self.socket_path);
        // 最后删自己。删掉之后进程仍在运行（inode 还在），由调用方要求退出。
        let _ = std::fs::remove_file(xt_proto::HELPER_INSTALLED_PATH);
    }

    /// **站点主体（可注入）**：两处回滚结果与「清理痕迹」都由参数给。
    ///
    /// 生产由 [`Helper::uninstall`] 传真实取值器；测试传注入结果 + 空清理 ⇒ **在完全不碰
    /// 本机路由/DNS/系统文件**的前提下，驱动**站点自己的代码**并断言最终 `Response`
    /// （这正是 `task-163` 要堵的 `n2`：站点自造成功响应）。
    fn uninstall_with<R, F, C>(
        &self,
        session_rollback: R,
        force_cleanup: F,
        remove_traces: C,
    ) -> Response
    where
        R: FnOnce() -> Result<(), String>,
        F: FnOnce() -> Result<(), String>,
        C: FnOnce(&Self),
    {
        let (rollback_failed, response) = uninstall_outcome(session_rollback(), force_cleanup());
        if let Some(why) = &rollback_failed {
            tracing::warn!(
                error = %why,
                "卸载时回滚网络配置失败 —— 路由/DNS 可能仍留在系统上"
            );
        }
        remove_traces(self);
        response
    }

}

/// **卸载结局 → 响应**（纯函数：四组输入都能行为级测，不必起真 helper）。
///
/// # 为什么把这段从站点里挪出来（task-160，采纳 tester 的 (a)）
///
/// `task-157` 实测：`fn uninstall` 的**文本守卫**能被三种「保留受检文本 + 运行时吞掉」
/// 绕过（响应前 `rollback_failed = None;` / 第二处分支 `let _ = e;` / 诱饵调用），
/// 而整个 `xt-helper` 仍然 **15/15 全绿** —— 根因是**站点没有任何行为测试**。
///
/// 抽成纯函数之后：
/// * 站点里**没有**「先抹掉失败、再拼文案」的中间变量（原来那种 V-1 写法编译不过）；
/// * 两处失败各自是否进文案，由下面的行为测试直接钉（V-2 的形状会红）；
/// * 响应**必定**是这里返回的那个（站点没有自己造文案的地方 ⇒ 诱饵 V-3 失去掩护）。
fn uninstall_outcome(
    rollback: Result<(), String>,
    force: Result<(), String>,
) -> (Option<String>, Response) {
    // 第一处（内存里那份会话）的失败**优先保留**：它更接近现场；`force_cleanup`
    // 只是补一刀（同一份快照在磁盘上再试一次）。但**任何一处失败都不许丢**。
    let why = rollback.err().or(force.err());
    let response = Response::Ok {
        message: Some(uninstall_response_message(why.as_deref())),
    };
    (why, response)
}

/// 卸载响应的文案（**纯函数**：两条路径都能行为级测，不必起真 helper）。
///
/// 回滚失败时必须**点名失败原因**，且**不许**出现「已回滚」这类会让用户
/// 以为网络已经回去的说法 —— 这正是 task-122 A-1/A-2 要守住的那条线。
fn uninstall_response_message(rollback_failed: Option<&str>) -> String {
    match rollback_failed {
        Some(why) => format!(
            "helper 已卸载；但回滚网络配置失败：{why} —— 请用「修复网络」再试一次"
        ),
        None => "helper 已卸载".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **没有活跃会话时必须拒绝安装信任锚。**
    ///
    /// 这条不需要 root：拒绝发生在碰系统钥匙串**之前**。
    /// 语义是"宁可拒绝，也不装"——因为信任锚必须挂在可回滚的会话快照上，
    /// 否则 helper 被 kill -9 之后，系统里会永久留下一个**没人记得**的根证书。
    #[test]
    fn installing_a_trust_anchor_without_a_session_is_refused_before_touching_the_keychain() {
        let helper = Helper::new(std::path::PathBuf::from("/tmp/xt-test.sock"));
        let response = helper.install_trust_anchor(
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
            "AB:CD:EF",
        );
        match response {
            Response::Error(e) => {
                assert_eq!(e.code, ErrorCode::InvalidRequest);
                assert!(
                    e.message.contains("TUN 会话"),
                    "错误必须说清为什么要先建隧道：{}",
                    e.message
                );
            }
            other => panic!("没有会话时居然不是拒绝：{other:?}"),
        }

        // 移除同理：没有会话就没有可回滚的依据，拒绝。
        match helper.remove_trust_anchor("AB:CD:EF") {
            Response::Error(e) => assert_eq!(e.code, ErrorCode::InvalidRequest),
            other => panic!("没有会话时移除也不该通过：{other:?}"),
        }
    }
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
    fn conflict_detection_uses_the_shared_predicate_and_has_a_boundary() {
        // helper **现在真正发出**的那条：结构化错误码（task-31）。
        let e = HelperError::new(
            ErrorCode::SessionConflict,
            "已有活跃会话 s-1（接口 utun4），请先 tun_down",
        );
        assert!(e.is_session_conflict());

        // 旧版 helper 的形态（兼容分支，见 xt_proto::HelperError::is_session_conflict）。
        let legacy = HelperError::new(
            ErrorCode::InvalidRequest,
            "已有活跃会话 s-1（接口 utun4），请先 tun_down",
        );
        assert!(legacy.is_session_conflict());

        // **边界**：同样是 InvalidRequest，但不是会话冲突就不能判为冲突 ——
        // 桌面端会据此去 Restore 拆会话，误判等于去拆一条不相干的会话。
        assert!(!invalid("别的问题").is_session_conflict());
        assert!(!HelperError::new(ErrorCode::Unauthorized, "已有活跃会话").is_session_conflict());
    }

    #[test]
    fn admin_gid_lookup_does_not_panic() {
        // 在真实 macOS 上 admin 组一定存在；这里只要求不 panic。
        let _ = admin_gid();
    }

    /// **站点级行为测试（task-163 主要修法）**：驱动**站点自己的代码**
    /// （`uninstall_with`，两处回滚结果 + 清理动作全部注入）并断言最终 `Response`。
    ///
    /// 为什么不直接 `dispatch(Request::Uninstall)`：那会**真的**跑 launchctl、删
    /// `/Library/...` 下的文件、并把磁盘上真实的会话快照回滚掉 —— 在开发机上等于
    /// 搞破坏（哪怕测试是以普通用户跑的，也不能写这种测试）。所以把「取值」与
    /// 「清理痕迹」做成接缝，**站点主体本身**照旧执行。
    /// 从 `dispatch` 到这里的几跳由下面的语义判据兜（那一层是文本层，见报告）。
    #[test]
    fn uninstall_site_response_is_honest_under_injected_failures() {
        fn message(r: &Response) -> String {
            match r {
                Response::Ok { message } => message.clone().unwrap_or_default(),
                other => panic!("卸载响应必须是 Response::Ok，实际 {other:?}"),
            }
        }
        let helper = Helper::new(PathBuf::from("/tmp/task-163-scratch.sock"));

        // ① 内存那份失败（注入）：站点必须如实说出来。
        let resp = helper.uninstall_with(
            || Err("删除路由 203.0.113.0/24 失败: route: not in table".into()),
            || Ok(()),
            |_| {},
        );
        let text = message(&resp);
        assert!(text.contains("回滚网络配置失败"), "{text}");
        assert!(text.contains("203.0.113.0/24"), "要点名具体失败步骤：{text}");
        assert_ne!(text, "helper 已卸载", "**n2 的形状**：站点自造成功响应必须在这里红");

        // ② 只有 force_cleanup 那一路失败（n3 的形状）：也必须进文案。
        let resp = helper.uninstall_with(
            || Ok(()),
            || Err("强制清理失败：磁盘上的快照读不出来".into()),
            |_| {},
        );
        let text = message(&resp);
        assert!(text.contains("强制清理失败"), "第二处失败也必须进响应：{text}");

        // ③ 两处都成功 ⇒ 才是干净的「helper 已卸载」。
        let resp = helper.uninstall_with(|| Ok(()), || Ok(()), |_| {});
        assert_eq!(message(&resp), "helper 已卸载");
    }

    /// **站点级行为测试（task-160；L0/L1：断言的就是用户可见的响应体）**。
    ///
    /// `task-157` 的 V-1/V-2 之所以能「守卫绿、行为吞掉」，根因是**站点没有行为测试**。
    /// 这条直接驱动抽出来的纯函数，把四组组合的**响应文案**钉死。
    #[test]
    fn uninstall_outcome_never_hides_a_rollback_failure() {
        fn message(r: &Response) -> String {
            match r {
                Response::Ok { message } => message.clone().unwrap_or_default(),
                other => panic!("卸载响应必须是 Response::Ok，实际 {other:?}"),
            }
        }

        // ① 内存那份失败 ⇒ 必须在响应里、点名**具体失败步骤**、且不含「已回滚」。
        let (_why, resp) = uninstall_outcome(
            Err("删除路由 203.0.113.0/24 失败: route: not in table".into()),
            Ok(()),
        );
        let text = message(&resp);
        assert!(text.contains("回滚网络配置失败"), "{text}");
        assert!(text.contains("203.0.113.0/24"), "要点名具体失败步骤：{text}");
        assert!(!text.contains("已回滚"), "失败时不许说「已回滚」：{text}");
        assert_ne!(
            text, "helper 已卸载",
            "**V-1 的形状**：失败被抹掉后响应会退回成功文案 —— 这里必须红"
        );

        // ② **第二处（force_cleanup）失败也必须进响应** —— 这正是 V-2 吞掉的那条分支。
        let (_why, resp) = uninstall_outcome(Ok(()), Err("强制清理失败：磁盘上的快照读不出来".into()));
        let text = message(&resp);
        assert!(
            text.contains("强制清理失败"),
            "**V-2 的形状**：第二处失败不进文案 ⇒ 这里必须红：{text}"
        );
        assert!(!text.contains("已回滚"), "{text}");

        // ③ 两处都失败：第一处优先（更接近现场），但「有失败」这件事不许丢。
        let (_why, resp) = uninstall_outcome(Err("第一条失败".into()), Err("第二条失败".into()));
        let text = message(&resp);
        assert!(text.contains("第一条失败"), "{text}");
        assert!(!text.contains("第二条失败"), "只保留第一条（现场更近）：{text}");

        // ④ 两处都成功 ⇒ 这才是干净的「helper 已卸载」。
        let (_why, resp) = uninstall_outcome(Ok(()), Ok(()));
        assert_eq!(message(&resp), "helper 已卸载");
    }

    /// 上一轮那两条（格式化函数本身）保留：它们是**纯函数**的更细一层。
    #[test]
    fn uninstall_message_is_honest_when_rollback_fails() {
        let failed =
            uninstall_response_message(Some("删除路由 203.0.113.0/24 失败: route: not in table"));
        assert!(failed.contains("回滚网络配置失败"), "{failed}");
        assert!(failed.contains("203.0.113.0/24"), "{failed}");
        assert!(!failed.contains("已回滚"), "{failed}");
        assert!(failed.contains("修复网络"), "{failed}");

        let ok = uninstall_response_message(None);
        assert_eq!(ok, "helper 已卸载");
        assert!(!ok.contains("失败"), "{ok}");
    }

    /// **站点语义判据（task-163）**：只锚**语义**，不锚标识符/绑定名/`mut`/类型注解 ——
    /// 无害的形状变化（tester 的 `n1`/`n5`）**不许红**；「站点自造成功响应」（`n2`）**必须红**。
    ///
    /// 五个语义条件（见 `uninstall_site_is_semantic`）：
    /// ① **卸载族**（入口 / 主体 / 纯函数）里 `Response::Ok` 只允许出现在 `uninstall_outcome`，
    ///    且入口与主体里**一处都没有**；dispatch 的卸载分支必须**委托**、不许自己造响应；
    /// ② 那一处必须真的把**两处**失败算进来（`.err().or(` + 进文案函数）；
    /// ③ 站点主体**恰好一次**调用 `uninstall_outcome`，两个实参都**不是**字面 `Ok(`，
    ///    返回的是那次调用解构出的**第二个绑定**（名字动态取 ⇒ 改绑定名不受影响）；
    /// ④ 入口只允许是**一行委托**。
    #[test]
    fn uninstall_site_cannot_swallow_rollbacks_in_production_source() {
        let prod = production_source();
        assert!(uninstall_site_is_semantic(prod), "站点语义判据不成立（四个条件见函数注释）");

        // —— 正向：无害形状变化**不许**红 ——
        let n1 = prod.replace(
            "let (rollback_failed, response) = uninstall_outcome(",
            "let (mut rollback_failed, response) = uninstall_outcome(",
        );
        assert_ne!(n1, prod, "n1 fixture 必须真的改到生产源码");
        assert!(
            uninstall_site_is_semantic(&n1),
            "`mut` 是无害的形状变化，守卫不许红（tester 的 n1）"
        );

        // n5：把响应绑定改成**别名**（tester 的原文形状：改名 + 再赋回原名）。
        let n5 = prod.replace(
            "let (rollback_failed, response) = uninstall_outcome(",
            "let (rollback_failed, response_alias) = uninstall_outcome(",
        );
        let n5 = n5.replace(
            "        remove_traces(self);\n        response",
            "        let response = response_alias;\n        remove_traces(self);\n        response",
        );
        assert_ne!(n5, prod, "n5 fixture 必须真的改到生产源码");
        assert!(
            uninstall_site_is_semantic(&n5),
            "改绑定名/加别名是无害重构，守卫不许红（tester 的 n5）"
        );

        // —— 负例：必须红 ——
        // n2：站点**自造**一个成功响应（shadowing 掉真响应）。
        let n2 = prod.replace(
            "        remove_traces(self);\n        response",
            "        let honest = &response;\n        let _ = honest;\n        let response = Response::Ok {\n            message: Some(\"helper 已卸载\".to_string()),\n        };\n        remove_traces(self);\n        response",
        );
        assert_ne!(n2, prod, "n2 fixture 必须真的改到生产源码");
        assert!(
            !uninstall_site_is_semantic(&n2),
            "站点自造响应必须红（tester 的 n2；全局只允许 uninstall_outcome 构造 Response::Ok）"
        );

        // 把两处结果换成假的成功（V-1new）。
        let v1 = prod.replace(
            "uninstall_outcome(session_rollback(), force_cleanup())",
            "uninstall_outcome(Ok(()), Ok(()))",
        );
        assert_ne!(v1, prod, "fixture 必须真的改到");
        assert!(!uninstall_site_is_semantic(&v1), "把两处结果换成假的 Ok 必须红");

        // 丢掉 force 那一路（V-2）。
        let v2 = prod.replace(
            "uninstall_outcome(session_rollback(), force_cleanup())",
            "uninstall_outcome(session_rollback(), Ok(()))",
        );
        assert_ne!(v2, prod, "fixture 必须真的改到");
        assert!(!uninstall_site_is_semantic(&v2), "force 那一路被丢掉必须红");

        // 常规吞法：`.ok()`（对照：task-134 已能抓）。
        let v3 = prod.replace(
            "controller::rollback(&session.snapshot).map_err(|e| e.to_string())",
            "controller::rollback(&session.snapshot).ok().map(|_| ()).map_err(|e| e.to_string())",
        );
        assert_ne!(v3, prod, "fixture 必须真的改到");
        assert!(!uninstall_site_is_semantic(&v3), "`.ok()` 这种吞法必须红");
    }

    /// 生产源码 = `server.rs` 去掉测试模块。
    fn production_source() -> &'static str {
        let src = include_str!("server.rs");
        match src.find("\n#[cfg(test)]\nmod tests") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    /// 语义判据本体（见上面那条测试的注释）。
    fn uninstall_site_is_semantic(src: &str) -> bool {
        // ① **卸载族**里 Response::Ok 只允许在 uninstall_outcome 体内（其它请求的
        //    dispatch 分支不算 —— 它们不在这条通路上）。
        let Some(outcome) = fn_body(src, "fn uninstall_outcome(") else {
            return false;
        };
        if outcome.matches("Response::Ok").count() != 1 {
            return false;
        }
        // ② 两处失败都必须进文案。
        let outcome_n = normalize(outcome);
        if !outcome_n.contains(".err().or(")
            || !outcome_n.contains("uninstall_response_message(")
        {
            return false;
        }
        // ③ 站点主体：恰好一次调用；两个实参都不是字面 Ok(；返回解构出的第二个绑定。
        let Some(site) = fn_body(src, "fn uninstall_with<R, F, C>(") else {
            return false;
        };
        let site_n = normalize(site);
        if site_n.matches("uninstall_outcome(").count() != 1 {
            return false;
        }
        let Some(args) = call_args(&site_n, "uninstall_outcome(") else {
            return false;
        };
        let parts = split_top(&args);
        if parts.len() != 2 || parts.iter().any(|a| a.trim_start().starts_with("Ok(")) {
            return false;
        }
        let Some(returned) = destructured_second_name(&site_n) else {
            return false;
        };
        // 站点返回的必须是**那次调用的响应绑定**（或它的别名）。
        // 先剥掉函数体收尾的 `}` 与可能的分号，再取最后一个标识符 ⇒ 不锚空白形状。
        let tail = site_n
            .trim_end()
            .trim_end_matches('}')
            .trim_end()
            .trim_end_matches(';')
            .trim_end();
        let tail_name = tail
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_'))
            .next()
            .unwrap_or("");
        let returned_ok = tail_name == returned
            // 别名：`let <尾名> = <第二个绑定>`（tester 的 n5 就是这种无害重构）
            || site_n.contains(&format!("let {tail_name}= {returned}"))
            || site_n.contains(&format!("let {tail_name} = {returned}"));
        if !returned_ok {
            return false;
        }
        // ④ 入口只允许一行委托；入口与主体里都不许出现 Response::Ok（= n2 的立足点）。
        let Some(entry) = fn_body(src, "fn uninstall(&self) -> Response") else {
            return false;
        };
        let entry_n = normalize(entry);
        if !(entry_n.contains("self.uninstall_with(") && !entry_n.contains("let ")) {
            return false;
        }
        if entry_n.contains("Response::Ok") || site_n.contains("Response::Ok") {
            return false;
        }
        // ⑥ 两处**取值器**不许在中间吞掉错误：错误必须活着到达 `uninstall_outcome`。
        let Some(collector) = fn_body(src, "fn rollback_memory_session(&self)") else {
            return false;
        };
        let collector_n = normalize(collector);
        if collector_n.contains(".ok()") || collector_n.contains("let _ = controller::rollback(") {
            return false;
        }
        if !collector_n.contains("controller::rollback(") || !entry_n.contains("map_err(") {
            return false;
        }
        // ⑤ dispatch 的卸载分支必须**委托**给 uninstall()，不许自己造响应。
        let Some(arm_at) = src.find("Request::Uninstall =>") else {
            return false;
        };
        let arm = match src[arm_at..].find('\n') {
            Some(i) => &src[arm_at..arm_at + i],
            None => &src[arm_at..],
        };
        arm.contains("self.uninstall()") && !arm.contains("Response::Ok")
    }

    /// 某个函数的**函数体**（花括号配对）。
    fn fn_body<'a>(src: &'a str, signature: &str) -> Option<&'a str> {
        let start = src.find(signature)?;
        let open = start + src[start..].find('{')?;
        let close = matching_brace(src, open)?;
        Some(&src[start..=close])
    }

    /// 从 `open`（一个 `{` 的下标）找配对的 `}`（朴素深度计数）。
    fn matching_brace(src: &str, open: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (i, c) in src[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(open + i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 去行注释 + 折叠空白 + 去掉 `mut `（形状噪音）⇒ 判据只锚语义。
    fn normalize(src: &str) -> String {
        src.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace("mut ", "")
    }

    /// 取调用 `name(` 的实参原文（配对括号内）。
    fn call_args(src: &str, name: &str) -> Option<String> {
        let start = src.find(name)? + name.len();
        let mut depth = 1usize;
        for (i, c) in src[start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(src[start..start + i].to_string());
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// 顶层逗号切分（忽略嵌套括号里的逗号）。
    fn split_top(args: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut cur = String::new();
        for c in args.chars() {
            match c {
                '(' | '[' | '{' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' | ']' | '}' => {
                    depth = depth.saturating_sub(1);
                    cur.push(c);
                }
                ',' if depth == 0 => out.push(std::mem::take(&mut cur)),
                _ => cur.push(c),
            }
        }
        if !cur.trim().is_empty() {
            out.push(cur);
        }
        out
    }

    /// `let (a, b) = …` 里的第二个名字（动态取 ⇒ 改绑定名不受影响）。
    fn destructured_second_name(src: &str) -> Option<String> {
        let p = src.find("let (")? + "let (".len();
        let close = src[p..].find(')')? + p;
        let names: Vec<String> = src[p..close].split(',').map(|s| s.trim().to_string()).collect();
        if names.len() != 2 {
            return None;
        }
        Some(names[1].clone())
    }
}
