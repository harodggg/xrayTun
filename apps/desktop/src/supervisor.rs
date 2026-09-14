//! 核心与隧道的生命周期编排。
//!
//! # 启动顺序是这里的全部难点
//!
//! TUN 模式下的正确顺序（每一步都有理由，不能随意调换）：
//!
//! ```text
//! 1. 探测物理出口（route -n get default）        ← 必须在建卡之前，否则拿到的是 utun
//! 2. helper.TunUp(defer=true)                     ← 建 utun + 配地址 + 装 bypass 路由
//! 3. helper.TakeTunFd                             ← SCM_RIGHTS 取回 fd
//! 4. 清除 fd 的 FD_CLOEXEC 并 spawn xray            ← 否则子进程继承不到 fd
//! 5. 等待 SOCKS 端口可连                            ← 唯一的、与版本无关的就绪信号
//! 6. helper.CommitRoutes                          ← 此时才接管默认路由 + 切 DNS
//! ```
//!
//! 第 6 步放在最后，是为了让「流量被接管」与「数据面可用」之间**没有窗口**。
//! 如果第 2 步就把默认路由接管了，第 5 步之前的几百毫秒里所有流量会被送进
//! 一个还没人读的 utun，同时 DNS 也已经指向隧道内的哨兵地址 ——
//! 用户看到的现象是「一连上所有网页都打不开」。
//!
//! 停止时顺序相反，并且**先杀数据面再回滚网络**：数据面还持有 utun fd，
//! 不先停掉的话接口不会消失，内核可能拒绝删除挂在它上面的路由。

use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use xt_core::model::{AppSettings, Node, ProxyMode};
use xt_core::xray::{
    self, CoreConfigInput, CoreEvent, InboundProfile, ProbeOptions, ProbeResult, XrayProcess,
    MIN_CORE_VERSION_NATIVE_TUN,
};
use xt_core::store::Store;
use xt_proto::{DatapathPlan, DefaultRouteMode, DnsMode, Request, TunUpRequest};

use crate::helper_client::HelperClient;
use crate::state::{profile_for, CoreRuntime};

/// 启动核心的总超时。超过这个时间认为「起不来了」，走回滚。
const CORE_READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 关核心时的优雅期，超过就 SIGKILL。
const CORE_SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// 探测代理服务器可达性的单次超时。
///
/// 不能太长：这是在启动路径上，用户正在等。
/// 也不能太短：跨国线路首次握手可能要 1 秒以上。
const REGION_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// 预览 TUN 启动将要做的网络改动（**只读，不改动任何东西**）。
///
/// 与 `start()` 走的是同一套计算：探测物理出口 → 解析服务器地址 →
/// 生成请求 → 编译成路由计划。所以预览出来的东西就是真会执行的东西。
pub fn preview_tun_plan(
    settings: &AppSettings,
    node: &Node,
) -> Result<xt_tun::plan::TunPlan, String> {
    let supervisor = Supervisor::default();
    let dr = xt_tun::macos::route::default_route().map_err(|e| e.to_string())?;
    let physical = xt_tun::plan::PhysicalUplink {
        interface: dr.interface.clone(),
        gateway: dr.gateway,
        service: xt_tun::macos::dns::service_for_device(&dr.interface).ok(),
    };
    let server_addrs = resolve_server_addrs(Some(node))?;
    let request = supervisor.build_tun_request(settings, physical.gateway, &server_addrs)?;
    xt_tun::plan::build_plan(&request, physical).map_err(|e| e.to_string())
}

/// 解析选中节点的服务器地址。
fn resolve_server_addrs(node: Option<&Node>) -> Result<Vec<std::net::IpAddr>, String> {
    let node = node.ok_or("TUN 模式必须选中一个节点")?;
    let addrs = xt_core::net::resolve_host(&node.address);
    if addrs.is_empty() {
        return Err(format!(
            "无法解析节点地址「{}」。TUN 模式需要先知道服务器的 IP 才能给它\n\
             单独留一条走物理出口的路由，否则会形成路由环。",
            node.address
        ));
    }
    Ok(addrs)
}

fn println_servers(addrs: &[std::net::IpAddr]) {
    let list = addrs.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ");
    tracing::info!(servers = %list, "已解析代理服务器地址，将为它们安装 bypass host 路由");
}

/// 异步版的可达性探测（把阻塞的 `connect` 丢到阻塞线程池，不卡住 runtime）。
async fn tcp_reachable(addr: std::net::SocketAddr, timeout: Duration) -> bool {
    tokio::task::spawn_blocking(move || xt_core::net::tcp_reachable(addr, timeout))
        .await
        .unwrap_or(false)
}

/// 去哪儿找 xray 可执行文件。
///
/// 这两个目录**类型完全一样**，作为相邻的位置参数极容易被写反 ——
/// 而写反之后的表现是「开发期找不到核心」，与「路径本身错了」难以区分
/// （见 docs/03 里 `CARGO_MANIFEST_DIR` 那次事故）。用具名结构体钉住。
#[derive(Debug, Clone, Default)]
pub struct CoreSearchPaths {
    /// **更新下来的核心**（用户数据目录下的 `core/`）。
    ///
    /// 优先于包内那份。更新故意不写进 `.app`（会破坏签名），
    /// 于是「回退到出厂版本」= 删掉这个目录。
    pub managed_core_dir: Option<PathBuf>,
    /// 打包后的资源目录（生产环境）。
    pub app_resource_dir: Option<PathBuf>,
    /// 开发期的 `apps/desktop/binaries`（由应用侧提供，核心不能自己猜）。
    pub dev_binaries_dir: Option<PathBuf>,
}

#[derive(Default)]
pub struct Supervisor {
    process: Option<XrayProcess>,
    /// 从 helper 拿到的 utun fd。
    ///
    /// **必须一直持有**：它是 utun 接口的存活凭证，关掉接口就消失。
    /// 子进程（xray）有自己的一份副本，但两份都关掉才会真正销毁接口。
    tun_fd: Option<RawFd>,
    session_id: Option<String>,
    physical_interface: Option<String>,
}

impl Supervisor {
    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn physical_interface(&self) -> Option<&str> {
        self.physical_interface.as_deref()
    }

    /// 启动核心（含可选的 TUN）。
    pub async fn start(
        &mut self,
        store: &Store,
        settings: &AppSettings,
        nodes: &[Node],
        helper: &mut HelperClient,
        events: Option<tokio::sync::mpsc::UnboundedSender<CoreEvent>>,
        paths: CoreSearchPaths,
    ) -> Result<CoreRuntime, String> {
        if self.process.is_some() {
            return Err("核心已经在运行".into());
        }

        // ---- 0) 前置检查：不要在改动系统之后才发现没得救 ----
        let selected = settings
            .selected_node
            .as_deref()
            .and_then(|id| nodes.iter().find(|n| n.id == id));
        if settings.mode != ProxyMode::Direct && selected.is_none() {
            return Err("请先选择一个节点".into());
        }

        let core_path = xray::resolve_core_binary(
            settings.core_path.as_deref(),
            paths.managed_core_dir.as_deref(),
            paths.app_resource_dir.as_deref(),
            paths.dev_binaries_dir.as_deref(),
        )
        .map_err(|e| e.to_string())?;
        let version = xray::core_version(&core_path).await.unwrap_or_default();
        let native_tun = version_meets(&version, MIN_CORE_VERSION_NATIVE_TUN);
        if settings.mode == ProxyMode::Tun && !native_tun {
            return Err(format!(
                "当前核心版本（{version}）不支持原生 TUN，需要 >= {MIN_CORE_VERSION_NATIVE_TUN}。\n\
                 请在「设置」里指定新的 xray 可执行文件。"
            ));
        }

        // ---- 1) 探测物理出口（必须在建 utun 之前） ----
        let physical = match settings.mode {
            ProxyMode::Tun => {
                let d = xt_tun::macos::route::default_route().map_err(|e| e.to_string())?;
                Some(d)
            }
            _ => None,
        };
        self.physical_interface = physical.as_ref().map(|d| d.interface.clone());

        // ---- 2) 生成并落盘配置 ----
        let profile: InboundProfile = profile_for(
            settings,
            self.physical_interface.as_deref(),
            native_tun,
        );
        let rules = xray::merge_rules(settings);
        let config = xray::build_pretty(&CoreConfigInput {
            settings,
            nodes,
            selected: selected.map(|n| n.id.as_str()),
            rules: &rules,
            profile,
            // 物理网卡：TUN 模式下 direct 出站要靠它逃出隧道。
            physical_interface: self.physical_interface.as_deref(),
        });
        let config_path = store.write_core_config(&config).map_err(|e| e.to_string())?;

        // 静态校验：把明显非法的配置挡在进程启动之前，错误信息也更可读。
        xray::validate_config(&core_path, &config_path)
            .await
            .map_err(|e| format!("生成的配置未通过核心自检：{e}"))?;

        // ---- 3) TUN：建卡 → 取 fd → 拉起核心 → 提交路由 ----
        let mut deferred_commit = false;
        // 在 TUN 分支里赋值、在第 6 步使用，所以必须在外层声明。
        let mut server_probe_target: Option<std::net::SocketAddr> = None;
        if settings.mode == ProxyMode::Tun {
            let gateway = physical.as_ref().and_then(|d| d.gateway);
            // 解析代理服务器地址 —— 防路由环的第一步，必须在接管默认路由之前做。
            let server_addrs = resolve_server_addrs(selected)?;
            println_servers(&server_addrs);

            let request = self.build_tun_request(settings, gateway, &server_addrs)?;
            // 用**节点的真实端口**探测，而不是随便挑一个端口。
            //
            // 提交前后都用同一个目标，于是两次探测构成一个干净的差分实验：
            // 唯一变化的是「默认路由被接管了」。前通后不通 → 一定是路由问题。
            // 换成别的端口就把「端口没开」和「路由环」混在一起了。
            server_probe_target = server_addrs
                .first()
                .map(|ip| std::net::SocketAddr::new(*ip, selected.map(|n| n.port).unwrap_or(443)));
            let session_id = request.session_id.clone();
            helper
                .tun_up(request)
                .map_err(|e| format!("helper 建立 TUN 失败：{}", e.message))?;
            self.session_id = Some(session_id.clone());

            let (info, fd) = helper
                .take_tun_fd(&session_id)
                .map_err(|e| format!("helper 交付 utun fd 失败：{}", e.message))?;
            tracing::info!(interface = %info.interface, fd, "已取得 utun fd");
            self.tun_fd = Some(fd);
            deferred_commit = true;
        }

        // ---- 4) 拉起核心 ----
        // geo 的退路：核心自己旁边没有就去包内资源目录 / 托管目录找。
        // 顺序与核心解析一致（包内优先于托管），但这里更宽松：
        // 只要哪个目录真的有 geo 文件就用哪个。
        let geo_fallback: Vec<PathBuf> = [paths.app_resource_dir.clone(), paths.managed_core_dir.clone()]
            .into_iter()
            .flatten()
            .collect();
        let process = spawn_core(&core_path, &config_path, self.tun_fd, events, &geo_fallback).await?;

        // ---- 5) 等待就绪 ----
        if let Err(e) = xray::wait_for_port(settings.socks_port, CORE_READY_TIMEOUT).await {
            let _ = process.shutdown(CORE_SHUTDOWN_GRACE).await;
            self.rollback_tun(helper);
            return Err(format!("核心未在预期时间内就绪：{e}"));
        }

        // ---- 6) 接管默认路由 + 切换 DNS ----
        //
        // 这一步是「有副作用的临界点」。之前任何失败都只影响我们自己，
        // 之后系统流量就真的进隧道了。所以两侧都要做可达性校验：
        //
        //   提交前 → 证明服务器本身是通的（排除节点配置错误）
        //   提交后 → 证明 bypass 路由真的生效（排除路由环）
        //
        // 第二次检查尤其关键：没有它，防环失效时会变成「显示已连接、
        // 但什么都打不开」—— 用户完全无从下手。有了它，启动直接失败并
        // 回滚，错误信息指向真正的原因。
        if deferred_commit {
            if let Some(target) = server_probe_target {
                if !tcp_reachable(target, REGION_PROBE_TIMEOUT).await {
                    let _ = process.shutdown(CORE_SHUTDOWN_GRACE).await;
                    self.rollback_tun(helper);
                    return Err(format!(
                        "接管默认路由之前就联系不上代理服务器 {target}。\n\
                         请检查节点地址 / 端口，以及本机到该服务器的直连是否正常。"
                    ));
                }
                tracing::info!(%target, "提交路由前：服务器可达");
            }

            let session_id = self.session_id.clone().unwrap_or_default();
            if let Err(e) = helper.call(&Request::CommitRoutes { session_id }) {
                let _ = process.shutdown(CORE_SHUTDOWN_GRACE).await;
                self.rollback_tun(helper);
                return Err(format!("接管默认路由失败（已回滚）：{}", e.message));
            }

            // 关键检查：默认路由已经指向隧道，此时**从本机**再连一次服务器。
            // 如果 bypass 路由没生效，这个连接会被送进隧道而永远出不去。
            if let Some(target) = server_probe_target {
                if !tcp_reachable(target, REGION_PROBE_TIMEOUT).await {
                    let _ = process.shutdown(CORE_SHUTDOWN_GRACE).await;
                    self.rollback_tun(helper);
                    return Err(format!(
                        "接管默认路由之后无法再联系代理服务器 {target} —— \
                         这是**路由环**：服务器自身的流量也被送进了隧道。\n\
                         原因通常是「服务器地址没有走物理出口的 host 路由」。\n\
                         已自动回滚，网络应已恢复。"
                    ));
                }
                tracing::info!(%target, "提交路由后：服务器仍可达（bypass 路由生效）");
            }
        }

        self.process = Some(process);

        Ok(CoreRuntime {
            running: true,
            pid: self.process.as_ref().and_then(|p| p.pid()),
            started_at_unix: Some(crate::state::now_unix()),
            config_path: Some(config_path),
            tun_session: self.session_id.clone(),
            tun_interface: self.physical_interface.clone().map(|_| "utun".to_string()),
            routes_committed: true,
            last_error: None,
        })
    }

    /// 停止核心并回滚隧道。
    pub async fn stop(&mut self, helper: &mut HelperClient) -> Result<(), String> {
        let mut errors: Vec<String> = Vec::new();

        // 先停数据面：它还持有 utun fd，不停掉接口不会消失。
        if let Some(process) = self.process.take() {
            if let Err(e) = process.shutdown(CORE_SHUTDOWN_GRACE).await {
                errors.push(format!("停止核心失败: {e}"));
            }
        }

        if let Err(e) = self.rollback_tun_inner(helper) {
            errors.push(e);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("；"))
        }
    }

    fn rollback_tun(&mut self, helper: &mut HelperClient) {
        if let Err(e) = self.rollback_tun_inner(helper) {
            tracing::error!(error = %e, "TUN 回滚失败，helper 侧快照已保留，下次启动会重试");
        }
    }

    fn rollback_tun_inner(&mut self, helper: &mut HelperClient) -> Result<(), String> {
        if let Some(session_id) = self.session_id.take() {
            let response = helper.call(&Request::TunDown { session_id });
            if let Err(e) = response {
                // 交给 helper 的 Restore 兜底：快照还在磁盘上。
                return Err(format!("helper 回滚 TUN 失败：{}", e.message));
            }
        }
        if let Some(fd) = self.tun_fd.take() {
            // SAFETY: fd 由本 struct 独占；关闭后 utun 接口（若无其它持有者）会消失。
            unsafe { libc::close(fd) };
        }
        self.physical_interface = None;
        Ok(())
    }

    pub fn build_tun_request(
        &self,
        settings: &AppSettings,
        physical_gateway: Option<std::net::IpAddr>,
        server_addrs: &[std::net::IpAddr],
    ) -> Result<TunUpRequest, String> {
        let tun = &settings.tun;

        // ---- 防环手段 1：代理服务器 IP 的 host 路由 ----
        //
        // 这是**必须**的，而且必须是第一条被安装的路由。
        //
        // 一度这里被留空过，理由是「Xray 的 `autoOutboundsInterface` 已经用
        // `IP_BOUND_IF` 把出站绑到物理网卡了」。那是错的，而且错得很隐蔽：
        //
        // `IP_BOUND_IF` 会把路由查找**限定在该接口上**。而我们的 `0.0.0.0/1`
        // 接管路由挂在 utun 上 —— 于是查找 203.0.113.10 时，最具体的匹配
        // 虽然在，却不在 en0 上，scoped 查找直接失败，返回 `ENETUNREACH`。
        //
        // 症状：隧道建起来了、路由也对、但核心连不上自己的服务器，
        // 日志里只有一句 `connect: network is unreachable`。
        //
        // 加上这个 host 路由之后，最具体的匹配就落在 en0 上了，
        // scoped 与 unscoped 两种查找都能成功。
        let mut bypass_hosts: Vec<std::net::IpAddr> = Vec::new();
        for ip in server_addrs {
            if !bypass_hosts.contains(ip) {
                bypass_hosts.push(*ip);
            }
        }
        // 物理网关本身也是「必须走物理出口的具体主机」。
        //
        // 放在 `bypass_hosts` 而不是 `bypass_networks`：语义上它是主机而不是
        // 网段，而且 `bypass_hosts` 会被**最先安装** —— 这正好是想要的顺序，
        // 网关可达是所有后续路由的前提。
        if let Some(gw) = physical_gateway {
            if !bypass_hosts.contains(&gw) {
                bypass_hosts.push(gw);
            }
        }
        if bypass_hosts.is_empty() {
            return Err(
                "无法确定代理服务器的 IP，不能安全地接管默认路由（会形成路由环）。\n\
                 请检查节点地址是否可解析。"
                    .to_string(),
            );
        }

        // ---- 防环手段 2：核心侧的出站接口绑定 ----
        // 保留它作为第二道保险：即使服务器换了 IP（域名的 A 记录变了），
        // `IP_BOUND_IF` 仍然能把出站钉在物理网卡上。
        // 两种手段的失效条件不重叠，所以两个都要有。
        let bypass = if tun.bypass_private {
            xt_proto::default_bypass_networks()
        } else {
            Vec::new()
        };


        Ok(TunUpRequest {
            // 会话 id 用「启动时刻的纳秒」而不是随机数：它天然单调，
            // 且出问题时能从日志时间反查到是哪一次启动。
            session_id: format!("s{}", now_nanos()),
            interface_name: None,
            mtu: tun.mtu,
            addresses: vec![tun
                .network_cidr()
                .ok_or_else(|| format!("非法 TUN 网段: {}", tun.network))?],
            routes: xt_proto::RoutePlan {
                default_route: DefaultRouteMode::SplitDefault,
                bypass_hosts,
                bypass_networks: bypass,
                ipv6: tun.ipv6,
            },
            dns: xt_proto::DnsPlan {
                mode: DnsMode::Automatic,
                // 哨兵地址：位于隧道网段内、不会被真实路由，唯一目的是
                // 让所有 53 端口流量必然进入隧道，从而被 Xray 的 dns-out 接管。
                servers: tun.sentinel_dns.parse().ok().into_iter().collect(),
                search_domains: Vec::new(),
            },
            // 数据面就是 Xray 自己（配置里已有 `protocol: "tun"` 入站），
            // helper 不需要再拉起任何东西。
            datapath: DatapathPlan::HandoffFd,
            defer_default_routes: true,
        })
    }
}

async fn spawn_core(
    core_path: &Path,
    config_path: &Path,
    tun_fd: Option<RawFd>,
    events: Option<tokio::sync::mpsc::UnboundedSender<CoreEvent>>,
    // geo_fallback_dirs：核心自己旁边没有 geo 文件时，退到这些目录里找。
    geo_fallback_dirs: &[PathBuf],
) -> Result<XrayProcess, String> {
    let mut envs: Vec<(&str, String)> = Vec::new();

    // geoip.dat / geosite.dat 的位置。
    //
    // 核心会在运行期读这两个文件来解析 `geoip:cn` / `geosite:cn` 规则，
    // 而它的工作目录是 runtime 目录（配置文件所在处），不是核心二进制所在处。
    // 不用这个环境变量的话，缺失时的行为是**规则静默不命中** ——
    // 日志里没有任何错误，用户只会看到「绕过大陆」预设完全没起作用。
    //
    // **注意 geo 和核心是两份独立的更新**：只更新了核心、托管目录里没有 geo
    // 文件是完全正常的状态。这时如果直接把 ASSET 指向托管目录，规则就会
    // 静默失效 —— 所以这里按「谁真的有 geo 文件」来选，而不是按「谁提供了核心」。
    let has_geo = |d: &std::path::Path| d.join("geosite.dat").is_file() || d.join("geoip.dat").is_file();
    let asset_dir = core_path
        .parent()
        .filter(|d| has_geo(d))
        .map(|d| d.to_path_buf())
        .or_else(|| geo_fallback_dirs.iter().find(|d| has_geo(d)).cloned());
    match asset_dir {
        Some(dir) => {
            tracing::debug!(dir = %dir.display(), "geo 数据目录");
            envs.push(("XRAY_LOCATION_ASSET", dir.display().to_string()));
        }
        None => tracing::warn!(
            "找不到 geoip.dat / geosite.dat，geoip:/geosite: 规则将不会命中（分流会静默失效）"
        ),
    }

    if let Some(fd) = tun_fd {
        // `xt_proto::transport` 收到 fd 时会设 CLOEXEC（避免泄漏给无关子进程），
        // 所以这里必须显式清掉，否则核心继承不到它。
        clear_cloexec(fd).map_err(|e| format!("清理 fd 的 FD_CLOEXEC 失败: {e}"))?;
        // Xray 同时接受 `xray.tun.fd` 与 `XRAY_TUN_FD`，两个都给上更保险。
        // 传给核心的编号就是 fd 自身的编号（`Command` 不会重排 fd）。
        envs.push(("XRAY_TUN_FD", fd.to_string()));
        envs.push(("xray.tun.fd", fd.to_string()));
    }

    XrayProcess::spawn_with_env(core_path, config_path, events, &envs)
        .await
        .map_err(|e| e.to_string())
}

fn clear_cloexec(fd: RawFd) -> std::io::Result<()> {
    // SAFETY: fcntl 只操作标志位。
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 探测一批节点的延迟，直到 `deadline` 为止。
pub async fn probe(
    nodes: &[Node],
    core_path: &Path,
    timeout: Duration,
) -> Result<Vec<ProbeResult>, String> {
    let opts = ProbeOptions {
        binary: core_path.to_path_buf(),
        timeout,
        ..Default::default()
    };
    xray::probe_nodes(nodes, &opts, None).await.map_err(|e| e.to_string())
}

/// 解析 `Xray 26.1.31 (go1.24.0 ...)` 这类版本串，判断是否 >= `min`。
fn version_meets(version_output: &str, min: &str) -> bool {
    let Some(found) = extract_version(version_output) else {
        return false;
    };
    compare_versions(&found, min) != std::cmp::Ordering::Less
}

fn extract_version(text: &str) -> Option<String> {
    // "Xray 26.1.31 (go1.24.0 darwin/arm64)" -> "26.1.31"
    for token in text.split_whitespace() {
        let cleaned = token.trim_start_matches('v');
        // 形如 "26.1.31"：至少一个点、以数字开头、其余只含数字与点。
        let looks_like_version = cleaned.split('.').count() >= 2
            && cleaned.chars().next().is_some_and(|c| c.is_ascii_digit())
            && cleaned.chars().all(|c| c.is_ascii_digit() || c == '.');
        if looks_like_version {
            return Some(cleaned.to_string());
        }
    }
    None
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..va.len().max(vb.len()) {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// 把两次采样换算成速率。
///
/// 单独抽出来是因为它有三个容易写错的点：第一次采样（`previous` 全 0）会
/// 产生一个虚假的峰值、计数器回绕（接口重开后）会得到巨大负值、
/// 以及 `elapsed` 很短时会把噪声放大成几百 MB/s。
pub fn rate_from(
    previous: &crate::state::TrafficSample,
    rx: u64,
    tx: u64,
    elapsed: Duration,
) -> crate::state::TrafficSample {
    let secs = elapsed.as_secs_f64();
    // 采样间隔过短（<100ms）就不算速率，避免把调度抖动放大成带宽尖峰。
    let (rx_rate, tx_rate) = if secs < 0.1 {
        (0, 0)
    } else {
        (
            ((rx.saturating_sub(previous.rx_bytes)) as f64 / secs) as u64,
            ((tx.saturating_sub(previous.tx_bytes)) as f64 / secs) as u64,
        )
    };
    crate::state::TrafficSample { rx_bytes: rx, tx_bytes: tx, rx_rate, tx_rate }
}

/// 等待核心可执行文件出现（用户在设置里改路径后的校验用）。
#[allow(dead_code)]
pub async fn wait_for_core_file(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    path.is_file()
}

/// 供 UI 显示的「是否支持原生 TUN」。
pub fn core_supports_native_tun(version: &str) -> bool {
    version_meets(version, MIN_CORE_VERSION_NATIVE_TUN)
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::{AppSettings, ProxyMode};

    /// 回归测试：TUN 请求**必须**把代理服务器 IP 放进 `bypass_hosts`。
    ///
    /// 这个字段一度被留空，理由是「核心的 `autoOutboundsInterface` 已经用
    /// `IP_BOUND_IF` 绑定了物理网卡」。结果 `IP_BOUND_IF` 会把路由查找限定在
    /// en0 上，而 `0.0.0.0/1` 挂在 utun 上 —— 于是核心连自己的服务器都
    /// 连不上，报 `network is unreachable`，隧道「建起来了但什么都打不开」。
    #[test]
    fn tun_request_always_bypasses_the_server_ip() {
        let sup = Supervisor::default();
        let settings = AppSettings { mode: ProxyMode::Tun, ..Default::default() };
        let server: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        let gw: std::net::IpAddr = "192.168.0.1".parse().unwrap();

        let req = sup.build_tun_request(&settings, Some(gw), &[server]).unwrap();
        assert!(
            req.routes.bypass_hosts.contains(&server),
            "服务器 IP 必须在 bypass_hosts 里，否则会形成路由环"
        );
        assert!(req.routes.bypass_hosts.contains(&gw), "物理网关也要能直连");
    }

    /// 拿不到服务器 IP 时必须**拒绝启动**，而不是硬着头皮接管默认路由。
    #[test]
    fn tun_request_refuses_without_server_ips() {
        let sup = Supervisor::default();
        let settings = AppSettings { mode: ProxyMode::Tun, ..Default::default() };
        let err = sup.build_tun_request(&settings, None, &[]);
        assert!(err.is_err(), "没有服务器 IP 就不能接管默认路由");
        let msg = err.unwrap_err();
        assert!(msg.contains("路由环"), "错误信息要说明后果：{msg}");
    }

    /// 跨 crate 断言：服务器 host 路由必须排在 `/1` 接管路由**之前**。
    ///
    /// 顺序反了的话，中间会有一个窗口「默认流量已进隧道、但服务器的包
    /// 还被送进隧道」，也就是路由环。plan.rs 里也测了同一件事，
    /// 但那用的是合成数据 —— 这里用的是真实的节点地址。
    #[test]
    fn server_host_route_precedes_default_capture_in_real_request() {
        let sup = Supervisor::default();
        let settings = AppSettings { mode: ProxyMode::Tun, ..Default::default() };
        let server: std::net::IpAddr = "203.0.113.10".parse().unwrap();
        let gw: std::net::IpAddr = "192.168.0.1".parse().unwrap();
        let req = sup.build_tun_request(&settings, Some(gw), &[server]).unwrap();

        let physical = xt_tun::plan::PhysicalUplink {
            interface: "en0".into(),
            gateway: Some(gw),
            service: Some("Wi-Fi".into()),
        };
        let plan = xt_tun::plan::build_plan(&req, physical).unwrap();

        let dests: Vec<String> = plan.routes.iter().map(|r| r.destination.to_string()).collect();
        let host_idx = dests.iter().position(|d| d == "203.0.113.10/32");
        let split_idx = dests.iter().position(|d| d == "0.0.0.0/1");

        assert!(host_idx.is_some(), "plan 里必须有服务器 host 路由：{dests:?}");
        assert!(split_idx.is_some(), "plan 里必须有 /1 接管路由");
        assert!(
            host_idx.unwrap() < split_idx.unwrap(),
            "服务器 host 路由必须先于 /1 接管安装：{dests:?}"
        );
    }

    #[test]
    fn version_comparison_handles_real_output() {
        let sample = "Xray 26.1.31 (go1.24.0 darwin/arm64)";
        assert!(version_meets(sample, "26.1.31"));
        assert!(version_meets(sample, "26.1.18"));
        assert!(!version_meets(sample, "26.2.0"));
        assert!(!version_meets("Xray 25.12.8 (go1.23)", "26.1.31"));
    }

    #[test]
    fn version_extraction_is_strict() {
        assert_eq!(extract_version("Xray 26.1.31 (go1.24.0)"), Some("26.1.31".into()));
        assert_eq!(extract_version("v26.9.9"), Some("26.9.9".into()));
        assert_eq!(extract_version("no version here"), None);
        // go1.24.0 含有 'o'，不应被误认为版本号
        assert_eq!(extract_version("go1.24.0"), None);
    }

    #[test]
    fn version_comparison_pads_short_versions() {
        assert_eq!(compare_versions("26.1", "26.1.0"), std::cmp::Ordering::Equal);
        assert_eq!(compare_versions("26.1.1", "26.1"), std::cmp::Ordering::Greater);
        assert_eq!(compare_versions("26", "26.0.1"), std::cmp::Ordering::Less);
    }

    #[test]
    fn unknown_version_is_treated_as_unsupported() {
        assert!(!version_meets("", "26.1.31"));
        assert!(!version_meets("garbage", "26.1.31"));
    }

    #[test]
    fn rate_from_ignores_short_intervals_and_counter_resets() {
        use crate::state::TrafficSample;
        let prev = TrafficSample { rx_bytes: 1000, tx_bytes: 2000, rx_rate: 0, tx_rate: 0 };

        // 采样间隔过短 → 不报速率
        let r = rate_from(&prev, 2000, 4000, Duration::from_millis(10));
        assert_eq!(r.rx_rate, 0);
        assert_eq!(r.rx_bytes, 2000);

        // 正常间隔 → 速率正确
        let r = rate_from(&prev, 3000, 6000, Duration::from_secs(2));
        assert_eq!(r.rx_rate, 1000);
        assert_eq!(r.tx_rate, 2000);

        // 计数器回绕（接口重开）→ saturating_sub 兜住，不会得到天文数字
        let r = rate_from(&prev, 10, 20, Duration::from_secs(1));
        assert_eq!(r.rx_rate, 0);
    }

    #[test]
    fn core_supports_native_tun_matches_min_version() {
        assert!(core_supports_native_tun("Xray 26.9.9 (go1.24.0)"));
        assert!(!core_supports_native_tun("Xray 26.1.18 (go1.24.0)"));
    }
}
