//! xt-bus 的行为测试：主题过滤、多订阅者、watch 视图、关闭语义。

use std::time::Duration;

use xt_bus::Bus;
use xt_contract::model::{ConnectionView, LogLevel, LogLine, Stage, Topic};
use xt_contract::protocol::Event;

fn log_event(message: &str) -> Event {
    Event::Log {
        line: LogLine {
            ts_ms: 1,
            level: LogLevel::Info,
            target: "xt-bus-test".to_string(),
            message: message.to_string(),
        },
    }
}

fn connected_view(since_ms: u64) -> ConnectionView {
    ConnectionView {
        stage: Stage::Connected,
        connected_since_ms: Some(since_ms),
        ..ConnectionView::default()
    }
}

#[tokio::test]
async fn subscribe_delivers_only_subscribed_topics() {
    let bus = Bus::new(16);
    let mut logs = bus.subscribe(&[Topic::Log]);
    let mut states = bus.subscribe(&[Topic::State]);

    bus.publish(Event::State { view: connected_view(1) });
    bus.publish(log_event("hello"));

    match states.recv().await {
        Some(Event::State { view }) => assert_eq!(view.stage, Stage::Connected),
        other => panic!("State 订阅者收到了 {other:?}"),
    }
    match logs.recv().await {
        Some(Event::Log { line }) => assert_eq!(line.message, "hello"),
        other => panic!("Log 订阅者收到了 {other:?}"),
    }
}

#[tokio::test]
async fn empty_subscription_receives_nothing() {
    let bus = Bus::new(8);
    let mut nothing = bus.subscribe(&[]);
    bus.publish(log_event("ignored"));
    let got = tokio::time::timeout(Duration::from_millis(50), nothing.recv()).await;
    assert!(got.is_err(), "空主题订阅不应收到任何事件");
}

#[tokio::test]
async fn every_subscriber_gets_its_own_copy() {
    let bus = Bus::new(8);
    let mut a = bus.subscribe(&[Topic::Log]);
    let mut b = bus.subscribe(&[Topic::Log]);
    bus.publish(log_event("fanout"));

    for stream in [&mut a, &mut b] {
        match stream.recv().await {
            Some(Event::Log { line }) => assert_eq!(line.message, "fanout"),
            other => panic!("订阅者收到了 {other:?}"),
        }
    }
}

#[tokio::test]
async fn publish_without_subscribers_is_not_an_error() {
    let bus = Bus::new(4);
    bus.publish(log_event("nobody listening"));
    bus.publish(Event::State { view: connected_view(3) });
}

#[tokio::test]
async fn connection_view_updates_even_without_watchers() {
    let bus = Bus::new(4);
    assert_eq!(bus.connection().stage, Stage::Disconnected);
    bus.set_connection(connected_view(42));
    // 没有 watcher 时也必须更新：否则 connection() 会返回陈旧快照。
    assert_eq!(bus.connection(), connected_view(42));
}

#[tokio::test]
async fn watch_connection_notifies_on_change() {
    let bus = Bus::new(8);
    let mut rx = bus.watch_connection();
    assert_eq!(*rx.borrow(), ConnectionView::default());

    bus.set_connection(connected_view(7));
    rx.changed().await.unwrap();
    assert_eq!(*rx.borrow(), connected_view(7));
    assert_eq!(bus.connection(), connected_view(7));
}

#[tokio::test]
async fn dropping_bus_closes_streams() {
    let bus = Bus::new(4);
    let mut stream = bus.subscribe(&[Topic::Log]);
    drop(bus);
    assert!(stream.recv().await.is_none(), "总线关闭后 recv 必须是 None");
}

#[tokio::test]
async fn slow_subscriber_loses_frames_but_keeps_working() {
    // capacity=1：发布 5 条、其间不读，订阅端必然丢帧；丢帧后仍要能收到最新一条，
    // 而不是卡死或返回 None。
    let bus = Bus::new(1);
    let mut stream = bus.subscribe(&[Topic::Log]);
    for i in 0..5 {
        bus.publish(log_event(&format!("m{i}")));
    }
    match stream.recv().await {
        Some(Event::Log { line }) => assert_eq!(line.message, "m4"),
        other => panic!("慢订阅端应拿到最新一帧，实得 {other:?}"),
    }
}
