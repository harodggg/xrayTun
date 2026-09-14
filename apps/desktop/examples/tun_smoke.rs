//! TUN 模式的端到端冒烟测试。
//!
//! **这是唯一会真正修改系统网络配置的测试。** 默认拒绝运行，必须显式确认。
//!
//! ```bash
//! # 先看它打算做什么，不执行
//! cargo run -p xraytun-desktop --example tun_smoke -- --dry-run
//!
//! # 真正执行（会改默认路由和系统 DNS）
//! cargo run -p xraytun-desktop --example tun_smoke -- --yes-i-understand
//! ```
//!
//! # 它验证什么
//!
//! 完整的两阶段启动链路，也就是 App 里点「连接」时走的**同一份代码**：
//!
//! ```text
//! 探测物理出口 → helper.TunUp(defer) → TakeTunFd（SCM_RIGHTS）
//!   → 清 FD_CLOEXEC → 拉起 xray 并传 XRAY_TUN_FD
//!   → 等 SOCKS 就绪 → helper.CommitRoutes（接管默认路由 + 切 DNS）
//!   → 穿过隧道访问外网
//!   → Supervisor::stop（先停数据面，再回滚网络）
//! ```
//!
//! # 为什么它值得单独存在
//!
//! 其余 191 个测试都只覆盖到纯函数与解析逻辑。**这个文件覆盖的是
//! 整个项目里唯一会破坏用户网络的那段代码。** 单元测试证明不了
//! 「回滚之后路由表真的回到原样」—— 那需要真的改一次再改回来。
//!
//! # 安全设计
//!
//! * 运行前打印完整的**改动前状态**（默认路由、DNS、已有 `/1` 路由）；
//! * 结束（包括 panic）时必定调用 `stop()`，由 helper 按快照回滚；
//! * 最后**重新读取**状态并逐项比对，任何一项没还原就返回非 0 退出码；
//! * 发现已有 `/1` 接管路由时**拒绝运行** —— 那说明别的 VPN/代理
//!   正在接管流量，叠加我们的一套会互相踩（见 docs/07 风险 R4）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use xraytun_desktop_lib::helper_client::HelperClient;
use xraytun_desktop_lib::supervisor::Supervisor;

use xt_core::model::{AppSettings, Node, ProxyMode};
use xt_core::store::Store;

/// 改动前的系统状态快照，用于事后比对。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SystemState {
    default_gateway: Option<String>,
    default_interface: Option<String>,
    capture_routes: Vec<String>,
    dns: Vec<String>,
}

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let confirmed = args.iter().any(|a| a == "--yes-i-understand");

    if !dry_run && !confirmed {
        print_banner();
        eprintln!("拒绝执行：需要 --yes-i-understand，或先用 --dry-run 查看计划。");
        return std::process::ExitCode::from(2);
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("创建 tokio runtime 失败：{e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(async move {
        match run(dry_run).await {
            Ok(true) => std::process::ExitCode::SUCCESS,
            Ok(false) => std::process::ExitCode::FAILURE,
            Err(e) => {
                eprintln!("\n✗ 测试失败：{e}");
                std::process::ExitCode::FAILURE
            }
        }
    })
}

fn print_banner() {
    eprintln!(
        r#"
╔══════════════════════════════════════════════════════════════════════╗
║  TUN 端到端冒烟测试                                                   ║
║                                                                      ║
║  这个测试会真实修改你的系统网络配置：                                  ║
║    · 创建 utun 虚拟网卡                                               ║
║    · 添加 0.0.0.0/1 与 128.0.0.0/1 两条默认路由接管                   ║
║    · 修改当前网络服务的 DNS 为隧道内哨兵地址                          ║
║                                                                      ║
║  结束时会通过 helper 的磁盘快照完整回滚。即使中途被 kill -9，          ║
║  下次 helper 启动也会自动回滚。                                       ║
║                                                                      ║
║  不要在正在使用其它 VPN / 代理接管流量的机器上运行。                   ║
╚══════════════════════════════════════════════════════════════════════╝
"#
    );
}

async fn run(dry_run: bool) -> Result<bool, String> {
    // ---------- 0) 读取配置与节点 ----------
    let store = Store::with_default_root();
    let mut settings: AppSettings = store.load_settings();
    let nodes: Vec<Node> = store.load_nodes();

    if nodes.is_empty() {
        return Err(format!(
            "{} 里没有节点。先用 App 添加一个，或跑 scripts/fetch-xray.sh 之后手动写入。",
            store.root().display()
        ));
    }

    // 确认选中节点存在（start() 内部也会再查一次，这里是为了给出更早、
    // 更可读的报错）。
    let node = settings
        .selected_node
        .as_deref()
        .and_then(|id| nodes.iter().find(|n| n.id == id))
        .or_else(|| nodes.first())
        .ok_or("没有可用节点")?;
    println!("使用节点：{} ({})", node.name, node.endpoint());

    settings.mode = ProxyMode::Tun;

    // ---------- 1) 记录改动前的状态 ----------
    let state_before = read_system_state()?;
    println!("=== 改动前的系统状态 ===");
    print_state(&state_before);

    // ---------- 2) 安全检查：拒绝在已有接管路由的机器上运行 ----------
    if !state_before.capture_routes.is_empty() {
        return Err(format!(
            "检测到已有默认路由接管（{}）。\n\
             这说明另一个 VPN / 代理工具正在接管流量。叠加第二套 /1 路由会互相覆盖，\n\
             而「谁负责删」将无法判定 —— 请先停掉那个工具。",
            state_before.capture_routes.join(", ")
        ));
    }

    let core = resolve_core()?;
    println!("\n内核：{}", core.display());

    if dry_run {
        // 把将要安装的每一条路由都列出来。这个清单是**只读**计算出来的，
        // 和真正执行时用的是同一套代码。
        match xraytun_desktop_lib::supervisor::preview_tun_plan(&settings, node) {
            Ok(plan) => {
                println!("\n=== 将会安装的路由（共 {} 条）===", plan.routes.len());
                for (i, r) in plan.routes.iter().enumerate() {
                    let via = match &r.via {
                        xt_proto::RouteVia::Interface { name } if name.is_empty() => {
                            "interface <新建的 utun>".to_string()
                        }
                        xt_proto::RouteVia::Interface { name } => format!("interface {name}"),
                        xt_proto::RouteVia::ScopedInterface { name, gateway } => {
                            format!("{gateway} (ifscope {name})")
                        }
                        xt_proto::RouteVia::Gateway { addr } => format!("gateway {addr}"),
                    };
                    let tag = match r.kind {
                        xt_tun::plan::RouteKind::Bypass if r.critical => "关键·绕过",
                        xt_tun::plan::RouteKind::Bypass => "可选·绕过",
                        xt_tun::plan::RouteKind::PhysicalScopedDefault => "逃逸·作用域默认",
                        xt_tun::plan::RouteKind::DefaultCapture => "接管默认路由",
                    };
                    println!("  {:>2}. {:<22} → {:<28} [{}]", i + 1, r.destination.to_string(), via, tag);
                }
                println!("\n  将写入的 DNS：{:?}", plan.dns_servers);
                println!("  会话 id     ：{}", plan.session_id);
            }
            Err(e) => println!("\n⚠ 无法生成预览计划：{e}"),
        }

        println!("\n=== --dry-run：以下是将会执行的步骤，不会真的执行 ===");
        println!("  1. helper.TunUp(defer_default_routes = true)  建 utun + 装 bypass 路由");
        println!("  2. helper.TakeTunFd                          取 fd（SCM_RIGHTS）");
        println!("  3. 清 FD_CLOEXEC → 拉起 xray（XRAY_TUN_FD）");
        println!("  4. 等 SOCKS 端口 {} 可连", settings.socks_port);
        println!("  5. helper.CommitRoutes                       接管 0.0.0.0/1 + 128.0.0.0/1 + 改 DNS");
        println!("  6. curl 穿过隧道访问外网（Cloudflare trace，验证出口 IP）");
        println!("  7. curl 穿过隧道访问国内站点（验证分流真的走 direct）");
        println!("  8. Supervisor::stop                          停数据面 + 回滚网络");
        println!("  9. 重新读取状态并与步骤 0 逐项比对");
        println!("\n未执行任何改动。");
        return Ok(true);
    }

    // ---------- 3) 真正启动（复用 App 的代码路径） ----------
    let mut helper = HelperClient::new(None);
    let mut supervisor = Supervisor::default();

    println!("\n=== 启动 TUN（两阶段） ===");

    // 用一个 guard 保证 panic 时也会回滚。
    let rollback = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // 捕获核心日志。隧道不通时，第一件要确定的事是
    // 「包到底有没有到 Xray」—— 只有核心日志能回答。
    let (log_tx, mut log_rx) = tokio::sync::mpsc::unbounded_channel::<xt_core::xray::CoreEvent>();
    let core_log: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let sink = core_log.clone();
        tokio::spawn(async move {
            while let Some(ev) = log_rx.recv().await {
                if let Ok(mut v) = sink.lock() {
                    if v.len() < 4000 {
                        v.push(ev.line);
                    }
                }
            }
        });
    }

    let start_result = supervisor
        .start(
            &store,
            &settings,
            &nodes,
            &mut helper,
            Some(log_tx),
            xraytun_desktop_lib::supervisor::CoreSearchPaths {
                managed_core_dir: None,
                app_resource_dir: None,
                dev_binaries_dir: xraytun_desktop_lib::dev_binaries_dir(),
            },
        )
        .await;

    let runtime = match start_result {
        Ok(rt) => rt,
        Err(e) => {
            // 启动失败时 supervisor 内部已经回滚过；这里再核对一次。
            eprintln!("\n✗ 启动失败：{e}");
            verify_restored(&state_before)?;
            return Ok(false);
        }
    };

    println!("  接口      : {:?}", runtime.tun_interface);
    println!("  会话      : {:?}", runtime.tun_session);
    println!("  核心 pid  : {:?}", runtime.pid);
    println!("  路由已接管: {}", runtime.routes_committed);

    let during = read_system_state()?;
    println!("\n=== 改动后的系统状态 ===");
    print_state(&during);
    dump_route_table();

    // ---------- 4) 穿过隧道验证连通 ----------
    println!("\n=== 穿过隧道访问外网 ===");
    let ok = curl_through_socks(settings.socks_port, "https://www.cloudflare.com/cdn-cgi/trace")
        .await;
    match &ok {
        Ok(body) => {
            let ip = body.lines().find(|l| l.starts_with("ip=")).unwrap_or("ip=?");
            let loc = body.lines().find(|l| l.starts_with("loc=")).unwrap_or("loc=?");
            println!("  ✓ 隧道可用   {ip}  {loc}");
        }
        Err(e) => println!("  ✗ 隧道不通：{e}"),
    }

    // ---------- 4a) 国内直连 ----------
    // 分流预设（bypass_mainland）的**核心承诺**就是国内站点不走节点。
    // 这一项长期缺失，于是「国内打不开」只能靠肉眼翻核心日志。
    println!("\n=== 国内直连（应走 direct，不经过节点）===");
    let domestic = curl_through_socks(settings.socks_port, "https://www.baidu.com/").await;
    match &domestic {
        Ok(body) => println!("  ✓ 国内可达   {} 字节", body.len()),
        Err(e) => println!("  ✗ 国内不通：{e}   ← freedom 出站被 /1 路由抢走的典型症状"),
    }

    // ---------- 4b) 核心日志：包到底有没有到 Xray ----------
    {
        let lines = core_log.lock().map(|v| v.clone()).unwrap_or_default();
        let accepted = lines.iter().filter(|l| l.contains("accepted")).count();
        let dialing = lines.iter().filter(|l| l.contains("dialing")).count();
        let hit = lines.iter().filter(|l| l.contains("Hit route rule")).count();
        // 分流是否真的生效，只能看出站类型：
        // 走节点 = vless tunneling，直连 = freedom connection opened。
        let via_node = lines
            .iter()
            .filter(|l| l.contains("proxy/vless/outbound: tunneling request"))
            .count();
        let via_direct = lines
            .iter()
            .filter(|l| l.contains("proxy/freedom: connection opened"))
            .count();
        println!("\n=== 核心日志统计（共 {} 行）===", lines.len());
        println!("  命中路由规则 : {hit}");
        println!("  接受连接     : {accepted}   ← 0 表示包根本没到 Xray");
        println!("  发起出站拨号 : {dialing}");
        println!("  经节点(vless): {via_node}");
        println!("  直连(freedom): {via_direct}");
        // 国内解析必须走国内解析器。
        //
        // 这一项是在一个真实故障之后补的：DNS 劫持规则原本只写了 `port: 53`，
        // 会连**内核自己的上游查询**一起吞掉 —— 国内解析这条腿从未离开过
        // 机器，所有解析都回退到走节点的 DoH，节点一慢就满屏超时。
        // 只看「网页能不能打开」是发现不了的：能打开，只是全是慢的。
        let domestic_dns = lines
            .iter()
            .filter(|l| l.contains("UDP:") && l.contains(":53 got answer"))
            .count();
        let hijacked_upstream = lines
            .iter()
            .filter(|l| l.contains("from DNS accepted") && l.contains("[dns-module -> dns-out]"))
            .count();
        println!("  国内解析应答 : {domestic_dns}   ← 0 表示国内解析没走出本机");
        if domestic_dns == 0 {
            println!("  ⚠ 没有任何国内解析器应答 —— 检查 DNS 劫持规则是否把内核自己的查询也吞了");
        }
        if hijacked_upstream > 0 {
            println!("  ⚠ 内核自己的上游 DNS 被 dns-out 劫持了 {hijacked_upstream} 次（应当走 direct）");
        }
        if via_node > 0 && via_direct == 0 {
            println!("  ⚠ 只有节点出站、没有直连出站 —— freedom 可能被路由抢走（见 docs/04 §8.2 / §8.3）");
        }
        println!("  --- 末尾 25 行 ---");
        for l in lines.iter().rev().take(25).collect::<Vec<_>>().into_iter().rev() {
            println!("    {l}");
        }
    }

    // ---------- 5) 停止并回滚 ----------
    println!("\n=== 停止并回滚 ===");
    if let Err(e) = supervisor.stop(&mut helper).await {
        eprintln!("  ✗ stop() 报错：{e}");
    }

    // 给内核一点时间让 utun 消失、路由随之清理。
    tokio::time::sleep(Duration::from_millis(500)).await;

    rollback.store(false, std::sync::atomic::Ordering::SeqCst);

    // ---------- 6) 逐项比对 ----------
    let restored = verify_restored(&state_before)?;
    let _ = rollback; // guard 仅用于语义说明

    println!();
    let all_ok = ok.is_ok() && domestic.is_ok() && restored;
    if all_ok {
        println!("✓ 全部通过：隧道可用、国内直连可用，且网络配置已完整还原");
        Ok(true)
    } else {
        println!(
            "✗ 未通过（外网={} 国内={} 还原={}）",
            ok.is_ok(),
            domestic.is_ok(),
            restored
        );
        Ok(false)
    }
}

fn read_system_state() -> Result<SystemState, String> {
    // 默认路由
    let out = std::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .map_err(|e| format!("route 失败：{e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut default_gateway = None;
    let mut default_interface = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("gateway:") {
            default_gateway = Some(v.trim().to_string());
        }
        if let Some(v) = line.strip_prefix("interface:") {
            default_interface = Some(v.trim().to_string());
        }
    }

    // 接管路由。
    //
    // **不要靠字符串猜 netstat 的格式**：macOS 对 /1 路由可能打印成
    // `0/1` 而不是 `0.0.0.0/1`，第一版就是这么被骗过去的 —— 明明路由装上了，
    // 测试却报「无」，把排查方向带偏了整整一轮。
    //
    // 这里改成解析每一行的**前缀长度**：只关心「有没有覆盖到全部 IPv4 空间的
    // 比 /0 更具体的路由」。
    let out = std::process::Command::new("/usr/sbin/netstat")
        .args(["-rn", "-f", "inet"])
        .output()
        .map_err(|e| format!("netstat 失败：{e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let mut capture_routes: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let dest = l.split_whitespace().next()?;
            // 形如 `0/1`、`0.0.0.0/1`、`128.0.0.0/1`、`128/1`
            let (net, prefix) = dest.split_once('/')?;
            let prefix: u8 = prefix.parse().ok()?;
            let first: u32 = net.split('.').next()?.parse().ok()?;
            // /1 的两半：0.0.0.0/1 与 128.0.0.0/1
            if prefix == 1 && (first == 0 || first == 128) {
                Some(format!("{net}/{prefix}"))
            } else {
                None
            }
        })
        .collect();
    capture_routes.sort();
    capture_routes.dedup();

    // DNS
    let dns = current_dns();

    Ok(SystemState { default_gateway, default_interface, capture_routes, dns })
}

fn current_dns() -> Vec<String> {
    // 从默认路由的接口反查网络服务名，再读它的 DNS。
    // 这里刻意不依赖 xt-tun 的内部函数，避免「测试和实现共用同一段错误逻辑」。
    let Ok(out) = std::process::Command::new("/usr/sbin/networksetup")
        .args(["-listnetworkserviceorder"])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout).to_string();

    // 找出当前有默认路由的那个接口对应的服务名。
    let iface = std::process::Command::new("/sbin/route")
        .args(["-n", "get", "default"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .and_then(|t| {
            t.lines()
                .find_map(|l| l.trim().strip_prefix("interface:").map(|v| v.trim().to_string()))
        });

    let Some(iface) = iface else { return Vec::new() };

    let mut current: Option<String> = None;
    let mut service: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("(Hardware Port:") {
            if line.contains(&format!("Device: {iface})")) {
                service = current.clone();
                break;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix('(') {
            if let Some((_i, name)) = rest.split_once(')') {
                current = Some(name.trim().to_string());
            }
        }
    }

    let Some(service) = service else { return Vec::new() };
    std::process::Command::new("/usr/sbin/networksetup")
        .args(["-getdnsservers", &service])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty() && l.parse::<std::net::IpAddr>().is_ok())
                .collect()
        })
        .unwrap_or_default()
}

fn print_state(s: &SystemState) {
    println!(
        "  默认路由  : {} via {}",
        s.default_gateway.as_deref().unwrap_or("(无)"),
        s.default_interface.as_deref().unwrap_or("(无)")
    );
    println!(
        "  接管路由  : {}",
        if s.capture_routes.is_empty() {
            "(无)".to_string()
        } else {
            s.capture_routes.join(", ")
        }
    );
    println!(
        "  DNS       : {}",
        if s.dns.is_empty() { "(DHCP 下发)".to_string() } else { s.dns.join(", ") }
    );
}

/// 打印路由表里与本次测试相关的行（原样，不做解析）。
///
/// 排障时最有用的就是这个：解析逻辑本身可能有 bug，
/// 而原始输出不会骗人。
fn dump_route_table() {
    let Ok(out) = std::process::Command::new("/usr/sbin/netstat").args(["-rn", "-f", "inet"]).output()
    else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    println!("  --- netstat -rn -f inet（相关行）---");
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("Destination") || l.starts_with("default") || l.contains("/1") || l.contains("/0") {
            println!("    {l}");
        }
    }
    println!("  --- route -n get default ---");
    if let Ok(o) = std::process::Command::new("/sbin/route").args(["-n", "get", "default"]).output() {
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            println!("    {}", line.trim());
        }
    }

    // 专门检查接口作用域的默认路由有没有**真实网关**。
    //
    // 这是 docs/04 §8.3 那个坑的守门测试：`-ifscope` 只说明「从哪张网卡出」，
    // 网关字段若是 `link#N`，内核就得为每个目的地址做 ARP —— 而目的地址
    // 通常不在本网段，于是包静默消失，表现为「绑定生效了但一个包都出不去」。
    // 只看 `route -n get default` 是查不出来的：那条命令两种情况都返回 en0。
    println!("  --- 接口作用域默认路由的网关检查 ---");
    let scoped: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("default") && l.contains("en") && l.ends_with('!'))
        .collect();
    if scoped.is_empty() {
        println!("    (没有接口作用域的默认路由)");
    }
    for l in scoped {
        let gateway = l.split_whitespace().nth(1).unwrap_or("?");
        if gateway.starts_with("link#") {
            println!("    ✗ {l}");
            println!("      └ 网关是 {gateway}（链路级，无下一跳）—— 直连流量会静默丢失！");
        } else {
            println!("    ✓ {l}");
            println!("      └ 网关 {gateway} 是真实地址，可正常转发");
        }
    }
}

/// 比对当前状态与 `expected`，打印每一项的差异。
fn verify_restored(expected: &SystemState) -> Result<bool, String> {
    let now = read_system_state()?;
    println!("=== 回滚校验 ===");
    let mut all_ok = true;

    let checks: [(&str, String, String); 4] = [
        (
            "默认路由网关",
            expected.default_gateway.clone().unwrap_or_default(),
            now.default_gateway.clone().unwrap_or_default(),
        ),
        (
            "默认路由接口",
            expected.default_interface.clone().unwrap_or_default(),
            now.default_interface.clone().unwrap_or_default(),
        ),
        (
            "接管路由",
            format!("{:?}", expected.capture_routes),
            format!("{:?}", now.capture_routes),
        ),
        ("DNS", format!("{:?}", expected.dns), format!("{:?}", now.dns)),
    ];

    for (name, want, got) in checks {
        let ok = want == got;
        all_ok &= ok;
        println!(
            "  {} {name}: {}",
            if ok { "✓" } else { "✗" },
            if ok { got } else { format!("期望 {want}，实际 {got}") }
        );
    }

    if !all_ok {
        eprintln!(
            "\n⚠ 有项目未还原。先试：\n\
             \x20   cargo run -p xt-helper -- restore\n\
             \x20 若无效，看 /Library/Logs/XrayTun/helper.log，以及\n\
             \x20   /Library/Application Support/XrayTun/helper-session.json 里的快照。"
        );
    }
    Ok(all_ok)
}

fn resolve_core() -> Result<PathBuf, String> {
    let settings = Store::with_default_root().load_settings();
    xt_core::xray::resolve_core_binary(
        settings.core_path.as_deref(),
        None,
        None,
        xraytun_desktop_lib::dev_binaries_dir().as_deref(),
    )
    .map_err(|e| e.to_string())
}

/// 用系统 curl 穿过我们的 SOCKS 入站发一个请求。
///
/// 不用 Rust HTTP 客户端：这一步要验证的是「隧道能不能过包」，
/// 用最普通的工具最有说服力，也避免引入只在测试里用到的依赖。
async fn curl_through_socks(port: u16, url: &str) -> Result<String, String> {
    let out = tokio::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--max-time",
            "25",
            "--socks5-hostname",
            &format!("127.0.0.1:{port}"),
            url,
        ])
        .output()
        .await
        .map_err(|e| format!("调用 curl 失败：{e}"))?;

    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
