//! xt-bus —— 进程内事件总线：broadcast 事件 + watch 连接视图，无轮询
//!
//! 所有者：backend-1。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 为什么是「总线 + 订阅」而不是「回调」：
//! 1. 回调会把「谁在等什么」写死在发布点。daemon 里同一个状态变化要同时送达
//!    IPC 连接、CLI、未来的本地订阅者，回调签名一定会长成一个万能参数包。
//! 2. 回调在发布线程里同步执行：一个慢消费者就能拖住整个 daemon。
//!    broadcast 的语义是「慢的人自己丢帧」，发布者永远不阻塞。
//! 3. 丢帧不是静默的：IPC 客户端按 `Event.seq` 检查连续性，跳号会被如实报出来。
//!
//! 这个 crate 不认识任何业务：它只做转发，不做判断。

use tokio::sync::{broadcast, watch};
use xt_contract::model::{ConnectionView, Topic};
use xt_contract::protocol::Event;

/// 进程内总线。daemon 里只创建一次，用 `Arc<Bus>` 共享。
pub struct Bus {
    events: broadcast::Sender<Event>,
    connection: watch::Sender<ConnectionView>,
}

impl Bus {
    /// `capacity` 是每个订阅者的帧缓冲上限。0 容量的通道会 panic 且毫无意义，
    /// 因此夹到 1：总线要么能存至少一帧，要么根本不该被订阅。
    pub fn new(capacity: usize) -> Bus {
        let (events, _) = broadcast::channel(capacity.max(1));
        let (connection, _) = watch::channel(ConnectionView::default());
        Bus { events, connection }
    }

    /// 发布一个事件。没有订阅者不是错误：事件总线的职责是「发出去」，
    /// 不是「保证有人听」。发送失败仅表示当前无人订阅，不改变任何状态。
    pub fn publish(&self, event: Event) {
        let _ = self.events.send(event);
    }

    /// 订阅若干主题。空列表是合法的：订阅方自己选择「什么都不听」。
    pub fn subscribe(&self, topics: &[Topic]) -> EventStream {
        EventStream { receiver: self.events.subscribe(), topics: topics.to_vec() }
    }

    /// 更新「当前连接视图」。`send_replace` 在没有 watcher 时也会更新值，
    /// 否则 `connection()` 会在无人监听时返回陈旧快照 —— 那就是假数据。
    pub fn set_connection(&self, view: ConnectionView) {
        let _ = self.connection.send_replace(view);
    }

    /// 当前连接视图。轮询是进不来的：要等变化请用 [`Bus::watch_connection`]。
    pub fn connection(&self) -> ConnectionView {
        self.connection.borrow().clone()
    }

    /// 连接视图的变化流。watch 只保留最新值，所以慢消费者拿到的是
    /// 「现在是什么」而不是「历史上每一帧」——这正是界面需要的东西。
    pub fn watch_connection(&self) -> watch::Receiver<ConnectionView> {
        self.connection.subscribe()
    }
}

/// 一个订阅端。`Option` 的 `None` 表示总线已被丢弃（进程收尾），
/// 而不是「暂时没有事件」——后者只会让 `recv` 挂起等待。
pub struct EventStream {
    receiver: broadcast::Receiver<Event>,
    topics: Vec<Topic>,
}

impl EventStream {
    /// 取下一个**本订阅关心**的事件。不关心的主题在这里就被丢掉，
    /// 不让它进入调用方的循环。
    pub async fn recv(&mut self) -> Option<Event> {
        loop {
            match self.receiver.recv().await {
                Ok(event) if self.topics.contains(&event.topic()) => return Some(event),
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    // 订阅方自己太慢导致的丢帧。不能静默：记下丢了多少，
                    // 让日志里能查到「为什么少了几帧」。这里不伪造事件。
                    tracing::warn!(dropped, "订阅端处理不过来，总线丢帧");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}
