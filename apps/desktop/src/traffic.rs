//! 实时网速：采样、换算、显示到标题栏与菜单栏。
//!
//! # 数据从哪来
//!
//! 从核心的 `StatsService` 读累计字节数（见 `xt_core::xray::stats`），
//! 两次采样的差值除以间隔就是速率。**不是**读网卡计数器 —— 那样在
//! 系统代理模式下会把无关流量也算进来。
//!
//! # 为什么要有这个模块
//!
//! 在此之前 `Inner::traffic` 从来没有人写过，面板上的速率永远是 0，
//! 而 UI 侧却一本正经地渲染着「累计 x 字节」。一个字段声明了却没人填，
//! 比没有这个字段更糟：它看起来是能用的。
//!
//! # 采样任务的生命周期
//!
//! 任务跟着核心一起生灭：`start_core` 成功后启动，`stop_core` 时 abort。
//! 不能让它活过核心 —— 核心一停，api 端口就没人监听，继续采样只会
//! 每秒产生一条连接失败日志。

use std::net::SocketAddr;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::events;
use crate::state::{AppState, TrafficSample};

/// 采样间隔。1 秒是「看起来实时」与「别把核心问烦」之间的平衡点。
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// 单次查询的超时。核心偶尔卡一下时，宁可这一拍没有读数，
/// 也不要让采样任务堆积。
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

/// 采样任务的把手。停止核心时用它把任务收掉。
pub struct TrafficMonitor {
    task: tokio::task::JoinHandle<()>,
}

impl TrafficMonitor {
    pub fn abort(&self) {
        self.task.abort();
    }
}

/// 启动采样任务。
pub fn spawn(app: AppHandle, api_port: u16) -> TrafficMonitor {
    // 用 tokio::spawn 而不是 tauri::async_runtime::spawn：后者的
    // JoinHandle 是 Tauri 自己的类型，`abort()` 拿不到。Tauri 的
    // 异步命令本来就跑在 tokio 上，这里和 start_core 里的日志转发
    // 任务用的是同一个运行时。
    let task = tokio::spawn(async move {
        let addr: SocketAddr = ([127, 0, 0, 1], api_port).into();
        let mut previous: Option<(u64, u64)> = None;

        loop {
            tokio::time::sleep(SAMPLE_INTERVAL).await;

            let stats = match xt_core::xray::query_stats(addr, QUERY_TIMEOUT).await {
                Ok(s) => s,
                Err(e) => {
                    // 采样失败不是致命错误：核心可能正在退出。记一条 debug
                    // 就够了，往 UI 日志里灌错误只会淹没真正有用的信息。
                    tracing::debug!(error = %e, "读取流量统计失败");
                    continue;
                }
            };

            let now = xt_core::xray::traffic_from_stats(&stats, API_TAG);
            let sample = match previous {
                Some((prx, ptx)) => TrafficSample {
                    rx_bytes: now.rx_bytes,
                    tx_bytes: now.tx_bytes,
                    rx_rate: rate(prx, now.rx_bytes),
                    tx_rate: rate(ptx, now.tx_bytes),
                },
                // 第一拍没有基准，速率只能是 0；累计值仍然有效。
                None => TrafficSample {
                    rx_bytes: now.rx_bytes,
                    tx_bytes: now.tx_bytes,
                    rx_rate: 0,
                    tx_rate: 0,
                },
            };
            previous = Some((now.rx_bytes, now.tx_bytes));

            apply(&app, &sample);
        }
    });

    TrafficMonitor { task }
}

/// 把一次采样写进状态与界面。
fn apply(app: &AppHandle, sample: &TrafficSample) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let show = state
        .with(|i| {
            i.traffic = sample.clone();
            i.settings.show_speed_in_title
        })
        .unwrap_or(false);

    update_titles(app, sample, show);
    events::runtime_changed(app, &state);
}

/// 更新窗口标题栏与菜单栏（托盘）标题。
///
/// **注意窗口标题其实是看不见的。** `tauri.conf.json` 里设了
/// `titleBarStyle: "Overlay"` + `hiddenTitle: true`，macOS 会隐藏原生标题文字，
/// 界面上那条顶栏是 webview 自己画的（`apps/ui/src/App.tsx` 的 `TopBar`）。
/// 所以真正给用户看网速的地方有两处：
///
/// * **顶栏** —— 由 `snapshot.traffic` 驱动，随 `runtime://changed` 每秒刷新；
/// * **菜单栏** —— 就是这里的 `tray.set_title`，关着窗口时唯一可见的读数。
///
/// `window.set_title` 仍然保留：它决定 Mission Control 与「窗口」菜单里显示
/// 什么，而且万一将来关掉 `hiddenTitle`，标题栏会立刻是对的。
pub fn update_titles(app: &AppHandle, sample: &TrafficSample, show: bool) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.set_title(&window_title(sample, show));
    }
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let text = if show { tray_title(sample) } else { String::new() };
        let _ = tray.set_title(Some(text.as_str()));
    }
}

/// 托盘图标 id，与 `tray::build` 里注册的一致。
const TRAY_ID: &str = "main-tray";

/// 我们自己轮询用的入站 tag，不参与网速计算。
const API_TAG: &str = "api";

/// 状态栏/标题栏里的**基础**标题，不显示网速时用它。
pub const APP_TITLE: &str = "XrayTun";

fn rate(previous: u64, current: u64) -> u64 {
    // 计数器在核心重启后会归零，那时 current < previous，
    // 直接相减会得到一个天文数字。按 0 处理。
    current.saturating_sub(previous) / SAMPLE_INTERVAL.as_secs()
}

/// 窗口标题栏：`XrayTun — ↓ 1.2 MB/s ↑ 34 KB/s`。
pub fn window_title(sample: &TrafficSample, show: bool) -> String {
    if !show {
        return APP_TITLE.to_string();
    }
    format!(
        "{APP_TITLE} — ↓ {} ↑ {}",
        format_rate(sample.rx_rate),
        format_rate(sample.tx_rate)
    )
}

/// 菜单栏：`↓1.2M ↑34K`。没有流量时留空，别在菜单栏常驻一串零。
pub fn tray_title(sample: &TrafficSample) -> String {
    if sample.rx_rate == 0 && sample.tx_rate == 0 {
        return String::new();
    }
    format!(
        "↓{} ↑{}",
        format_rate_compact(sample.rx_rate),
        format_rate_compact(sample.tx_rate)
    )
}

/// `1234` → `1.2 KB/s`。用 1024 进制，与面板上的 `formatBytes` 保持一致。
pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// 菜单栏用的紧凑写法：`1.2M`、`34K`。省掉空格和小数点后的零。
fn format_rate_compact(bytes_per_sec: u64) -> String {
    const K: f64 = 1024.0;
    let v = bytes_per_sec as f64;
    if v < K {
        format!("{bytes_per_sec}B")
    } else if v < K * K {
        format!("{:.0}K", v / K)
    } else if v < K * K * K {
        format!("{:.1}M", v / (K * K))
    } else {
        format!("{:.2}G", v / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(rx: u64, tx: u64) -> TrafficSample {
        TrafficSample { rx_bytes: rx, tx_bytes: tx, rx_rate: rx, tx_rate: tx }
    }

    #[test]
    fn formats_rates_with_binary_units() {
        assert_eq!(format_rate(0), "0 B/s");
        assert_eq!(format_rate(512), "512 B/s");
        assert_eq!(format_rate(1024), "1.0 KB/s");
        assert_eq!(format_rate(1536), "1.5 KB/s");
        assert_eq!(format_rate(1024 * 1024), "1.0 MB/s");
        assert_eq!(format_rate(3 * 1024 * 1024 * 1024), "3.0 GB/s");
    }

    #[test]
    fn compact_form_is_short() {
        assert_eq!(format_rate_compact(0), "0B");
        assert_eq!(format_rate_compact(999), "999B");
        assert_eq!(format_rate_compact(1024), "1K");
        assert_eq!(format_rate_compact(1536), "2K");
        assert_eq!(format_rate_compact(1024 * 1024 * 3 / 2), "1.5M");
        assert_eq!(format_rate_compact(1024 * 1024 * 1024 * 2), "2.00G");
    }

    #[test]
    fn window_title_shows_both_directions() {
        let t = window_title(&sample(1024 * 1024, 34 * 1024), true);
        assert_eq!(t, "XrayTun — ↓ 1.0 MB/s ↑ 34.0 KB/s");
    }

    #[test]
    fn window_title_falls_back_to_app_name_when_disabled() {
        assert_eq!(window_title(&sample(1024, 1024), false), "XrayTun");
    }

    #[test]
    fn tray_title_is_empty_when_idle() {
        // 空闲时菜单栏不该常驻一串 0 —— 那是纯噪音。
        assert_eq!(tray_title(&sample(0, 0)), "");
        assert_eq!(tray_title(&sample(0, 2048)), "↓0B ↑2K");
    }

    #[test]
    fn tray_title_is_shorter_than_window_title() {
        let s = sample(1024 * 1024 * 5, 1024 * 200);
        assert!(
            tray_title(&s).len() < window_title(&s, true).len(),
            "菜单栏写法必须比标题栏短"
        );
        assert_eq!(tray_title(&s), "↓5.0M ↑200K");
    }

    /// 计数器在核心重启后归零。若不设防，速率会变成 u64 级别的天文数字。
    #[test]
    fn counter_reset_does_not_produce_absurd_rates() {
        assert_eq!(rate(9_000_000, 100), 0);
        assert_eq!(rate(100, 9_000_000), 8_999_900);
    }
}
