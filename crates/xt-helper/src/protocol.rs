//! 传输层：直接复用 `xt-proto::transport`。
//!
//! 这一层之所以**没有**自己的实现，是因为 GUI 侧需要一模一样的代码。
//! 两边各写一份「长度前缀分帧 + `SCM_RIGHTS`」的实现，几乎必然出现
//! 「一边改了另一边没改」的 drift，而这类 drift 的故障现象
//! （帧错位、fd 掉到下一条消息上）极难定位。
//!
//! 所以实现放在共享 crate 里，这里只做转发。

// 二进制 crate 没有外部消费者，纯 `pub use` 会被判定为「未使用」。
// 保留这层转发本身就是设计意图，所以显式放行。
#![allow(unused_imports)]

pub use xt_proto::transport::{
    bind, connect, peer_audit_token, peer_credentials, peer_pid, recv, recv_fd, send, send_fd,
    set_cloexec,
};
