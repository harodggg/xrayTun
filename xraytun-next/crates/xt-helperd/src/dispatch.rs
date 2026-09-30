//! 指令分发：`xt_helperproto::Request` → `xt_macosnet` 调用。
//!
//! # 错误通道
//!
//! [`dispatch`] 的返回类型是 `Result<Outcome, ErrorBody>`：
//!
//! * `Ok(Outcome)` —— 业务响应（可能还要附带一个 fd，见 [`Outcome::fd`]）；
//! * `Err(ErrorBody)` —— 指令被拒。连接层把它包成
//!   [`xt_helperproto::Response::Error`] 写回一帧（见 [`crate::server`]）。
//!
//! 所以线上「每一条请求恰好得到一帧响应」，且失败带结构化的
//! `code` / `message`。dispatch 自己**不**造 `Response::Error` —— 这样
//! 「成功路径」与「失败路径」在类型上就分开了，不存在「忘了包错误」的写法。
//!
//! # 会话状态
//!
//! helperd 维护一份活跃会话引用（`std::sync::Mutex`），`TunUp` 时置、
//! `TunDown` 时清。`Status` 直接返回它；`TakeTunFd` / `CommitRoutes` /
//! `TunDown` 都必须携带与它一致的 [`SessionRef`]，否则拒绝 —— 这拦住了
//! 「陈旧请求误拆/误操作后来建立的新会话」。
//!
//! 锁只在同步调用前后短暂持有，**从不跨越 `.await`**（helperd 的处理是纯同步的）。

use std::os::fd::RawFd;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use xt_contract::error::{bad_request, conflict, internal, not_found, ErrorBody};
use xt_helperproto::{HELPER_PROTOCOL_VERSION, Request, Response, SessionRef, TunUpArgs};
use xt_macosnet::{TunRequest, TunSession};

/// `RestoreStale.state_dir` 的白名单前缀。
///
/// helper 以 root 运行，`state_dir` 决定「去哪里读快照、并据此改系统网络」——
/// 任意路径在这里就是提权面。`/Library/Application Support` 是生产位置；
/// `/tmp` 只为开发/测试（例如 `cargo test` 起的临时 helper）。
///
/// 匹配是**按路径分量**的（`Path::starts_with`），所以 `/tmpfoo` **不会**
/// 匹配 `/tmp`，`/Library/Application SupportX` 也不会匹配。
pub const ALLOWED_STATE_DIR_ROOTS: &[&str] =
    &["/Library/Application Support", "/tmp"];

/// helperd 侧记录的活跃会话。
#[derive(Debug)]
struct ActiveSession {
    session: SessionRef,
    /// TakeTunFd 之后由 helperd 持有的 utun fd。
    ///
    /// **要一直开着**：utun 接口的生命周期绑定在描述符上。TakeTunFd 之前 helperd
    /// 是唯一持有者，此时关闭接口就消失；TakeTunFd 之后数据面也拿到一份内核复制
    /// 出的副本，接口要等所有副本都关闭才销毁。所以这份不是「多余的」—— 它是
    /// helper 对接口的引用。关闭它的唯一时机是 `TunDown`。
    tun_fd: Option<RawFd>,
}

/// helperd 的进程内状态：当前活跃会话（一次只服务一个 TUN）。
#[derive(Debug, Default)]
pub struct State {
    active: Mutex<Option<ActiveSession>>,
}

/// 连接任务共享的状态句柄。
pub type SharedState = Arc<State>;

impl State {
    pub fn new() -> Self {
        State { active: Mutex::new(None) }
    }

    /// 当前活跃会话（`Status` 用）。锁中毒时返回 `None`：这只是状态查询，
    /// 不该因为一次中毒就 panic 掉整个守护进程。
    pub fn active_session(&self) -> Option<SessionRef> {
        match self.active.lock() {
            Ok(guard) => guard.as_ref().map(|active| active.session.clone()),
            Err(_) => None,
        }
    }

    fn set_active(&self, session: SessionRef) -> Result<(), ErrorBody> {
        let mut guard = self.active.lock().map_err(|_| internal("会话锁中毒"))?;
        *guard = Some(ActiveSession { session, tun_fd: None });
        Ok(())
    }

    fn clear_active(&self) -> Result<(), ErrorBody> {
        let mut guard = self.active.lock().map_err(|_| internal("会话锁中毒"))?;
        *guard = None;
        Ok(())
    }

    /// 校验请求里的会话引用与当前活跃会话一致。不一致一律 `NotFound`：
    /// 陈旧会话 id **不能**误操作当前会话。
    fn expect_active(&self, session: &SessionRef) -> Result<(), ErrorBody> {
        let guard = self.active.lock().map_err(|_| internal("会话锁中毒"))?;
        let active = guard
            .as_ref()
            .ok_or_else(|| not_found("没有活跃 TUN 会话"))?;
        if active.session.id != session.id {
            return Err(not_found(format!(
                "会话 id 不匹配（请求 {}，当前 {}）",
                session.id, active.session.id
            )));
        }
        Ok(())
    }

    /// 接管 utun fd 的所有权。同一会话重复交出 fd 是 bug（xt-macosnet 自己也会
    /// 拒绝），这里同样 fail-closed，绝不静默覆盖一个已持有的 fd。
    fn retain_fd(&self, fd: RawFd) -> Result<(), ErrorBody> {
        let mut guard = self.active.lock().map_err(|_| internal("会话锁中毒"))?;
        let active = guard
            .as_mut()
            .ok_or_else(|| not_found("没有活跃 TUN 会话"))?;
        if active.tun_fd.is_some() {
            return Err(conflict("该会话的 utun fd 已经交出过"));
        }
        active.tun_fd = Some(fd);
        Ok(())
    }

    /// 取走 helper 侧持有的 utun fd（`TunDown` 时关闭它）。
    fn take_fd(&self) -> Result<Option<RawFd>, ErrorBody> {
        let mut guard = self.active.lock().map_err(|_| internal("会话锁中毒"))?;
        Ok(guard.as_mut().and_then(|active| active.tun_fd.take()))
    }
}

/// 一次指令处理的结果。
///
/// `fd` 只在 `TakeTunFd` 时是 `Some`：连接层必须先写出 `response` 帧，
/// 再用 `SCM_RIGHTS` 把 fd 发出去（**帧先、fd 后**，见 [`crate::fdpass`]）。
#[derive(Debug)]
pub struct Outcome {
    pub response: Response,
    /// 响应帧之后要经 `SCM_RIGHTS` 发出的 fd。
    pub fd: Option<RawFd>,
}

impl Outcome {
    fn reply(response: Response) -> Self {
        Outcome { response, fd: None }
    }
}

/// 把一条封闭指令分发到 `xt-macosnet`。
///
/// 这里**没有**「默认分支」：`Request` 是穷举的封闭枚举，未知 `cmd` 在
/// `decode_request` 阶段就已经报错，根本到不了这里。
pub fn dispatch(state: &State, request: Request) -> Result<Outcome, ErrorBody> {
    match request {
        Request::Status => Ok(Outcome::reply(Response::Status {
            version: HELPER_PROTOCOL_VERSION,
            active_session: state.active_session(),
        })),
        Request::TunUp { args } => tun_up(state, args),
        Request::TakeTunFd { session } => take_tun_fd(state, session),
        Request::CommitRoutes { session } => {
            state.expect_active(&session)?;
            xt_macosnet::commit_routes(&to_macosnet(&session))?;
            tracing::info!(id = %session.id, "默认路由与 DNS 已接管");
            Ok(Outcome::reply(Response::Ok))
        }
        Request::TunDown { session } => {
            state.expect_active(&session)?;
            xt_macosnet::tun_down(&to_macosnet(&session))?;
            // 关闭 helper 侧保留的 fd：接口随最后一个 fd 关闭而销毁。
            if let Some(fd) = state.take_fd()? {
                // SAFETY: fd 是 TakeTunFd 时由 xt-macosnet 交给我们、由 State
                // 独占持有的；这里关闭它，且 State 已不再保存它 ⇒ 不会双重关闭。
                unsafe { libc::close(fd) };
            }
            state.clear_active()?;
            tracing::info!(id = %session.id, "TUN 会话已拆除");
            Ok(Outcome::reply(Response::Ok))
        }
        Request::RestoreStale { state_dir } => {
            // 启动时专用（见 helperproto 的字段注释）。内存里已有活跃会话时拒绝：
            // 否则会把一条「正在建立/正在运行」的会话在磁盘上回滚掉，而 helperd
            // 内存里还当它活着 —— 两份真相不一致，之后的 TunDown 会去拆不存在的东西。
            if let Some(existing) = state.active_session() {
                return Err(conflict(format!(
                    "已有活跃会话 {}（接口 {}），拒绝在此刻回滚磁盘快照",
                    existing.id, existing.interface
                )));
            }
            let dir = validate_state_dir_str(&state_dir)?;
            let interface = xt_macosnet::restore_stale(&dir)?;
            match &interface {
                Some(name) => tracing::warn!(interface = %name, "已回滚残留会话"),
                None => tracing::info!("没有需要回滚的残留会话"),
            }
            Ok(Outcome::reply(Response::RestoreStale { interface }))
        }
    }
}

/// `TunUp`：把线上词汇映射成 `xt_macosnet::TunRequest`，建卡并记住会话。
fn tun_up(state: &State, args: TunUpArgs) -> Result<Outcome, ErrorBody> {
    // 已有活跃会话：拒绝。并发的两张 TUN 会互相踩路由，且「谁负责回滚」无法判定。
    if let Some(existing) = state.active_session() {
        return Err(conflict(format!(
            "已有活跃会话 {}（接口 {}），请先 tun_down",
            existing.id, existing.interface
        )));
    }

    let request = TunRequest {
        addresses: args.addresses,
        mtu: args.mtu,
        bypass_routes: args.bypass_routes,
        default_routes: args.default_routes,
        dns_servers: args.dns_servers,
    };
    let session = xt_macosnet::tun_up(&request)?;
    let session = SessionRef { id: session.id, interface: session.interface };
    state.set_active(session.clone())?;
    tracing::info!(
        id = %session.id,
        interface = %session.interface,
        "TUN 会话已建立（等待 TakeTunFd / CommitRoutes）"
    );
    Ok(Outcome::reply(Response::TunUp { session }))
}

/// `TakeTunFd`：从 xt-macosnet 取 fd，转由 helperd 持有，并把 fd 交给连接层发送。
fn take_tun_fd(state: &State, session: SessionRef) -> Result<Outcome, ErrorBody> {
    state.expect_active(&session)?;
    let fd = xt_macosnet::take_fd(&to_macosnet(&session))?;
    // 所有权转入 helperd：fd 由 State 持有直到 TunDown，接口才不会消失。
    state.retain_fd(fd)?;
    tracing::info!(id = %session.id, fd, "utun fd 待交付（响应帧先、SCM_RIGHTS 后）");
    Ok(Outcome { response: Response::TakeTunFd { session }, fd: Some(fd) })
}

/// 校验 `RestoreStale` 的 `state_dir`：绝对路径、无 `..`、且落在白名单前缀内。
pub fn validate_state_dir(path: &Path) -> Result<(), ErrorBody> {
    if !path.is_absolute() {
        return Err(bad_request(format!(
            "state_dir 必须是绝对路径：{}",
            path.display()
        )));
    }
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(bad_request(format!(
            "state_dir 不允许含 '..'（路径穿越）：{}",
            path.display()
        )));
    }
    let allowed = ALLOWED_STATE_DIR_ROOTS.iter().any(|root| {
        let root = Path::new(root);
        path == root || path.starts_with(root)
    });
    if !allowed {
        return Err(bad_request(format!(
            "state_dir {} 不在白名单内（允许的前缀：{}）",
            path.display(),
            ALLOWED_STATE_DIR_ROOTS.join(" / ")
        )));
    }
    Ok(())
}

fn validate_state_dir_str(raw: &str) -> Result<PathBuf, ErrorBody> {
    let path = PathBuf::from(raw);
    validate_state_dir(&path)?;
    Ok(path)
}

/// 线上 `SessionRef` → `xt-macosnet::TunSession`。
fn to_macosnet(session: &SessionRef) -> TunSession {
    TunSession { id: session.id.clone(), interface: session.interface.clone() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 白名单的**判别性**：合法前缀通过，同前缀的「兄弟目录」与路径穿越必须被拒。
    #[test]
    fn state_dir_whitelist_has_a_boundary() {
        assert!(validate_state_dir(Path::new("/Library/Application Support/XrayTun")).is_ok());
        assert!(validate_state_dir(Path::new("/Library/Application Support")).is_ok());
        assert!(validate_state_dir(Path::new("/tmp/xraytun-test")).is_ok());

        // 兄弟目录：分量匹配，不许把 /tmpfoo 当成 /tmp。
        assert!(validate_state_dir(Path::new("/tmpfoo")).is_err());
        assert!(validate_state_dir(Path::new("/Library/Application SupportX")).is_err());
        // 任意路径与相对路径。
        assert!(validate_state_dir(Path::new("/etc")).is_err());
        assert!(validate_state_dir(Path::new("/Users/someone")).is_err());
        assert!(validate_state_dir(Path::new("relative/path")).is_err());
        // 路径穿越：即使前缀看着对，也必须拒。
        assert!(validate_state_dir(Path::new("/tmp/../etc")).is_err());
    }
}
