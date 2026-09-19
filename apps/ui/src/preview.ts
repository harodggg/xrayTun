/**
 * 浏览器预览桥接（**只用于开发**，绝不进生产路径）。
 *
 * # 为什么需要它
 *
 * 界面完全依赖 Tauri 的 `invoke`：在普通浏览器里打开 `vite dev` 只会看到
 * 一连串 "window.__TAURI_INTERNALS__ is undefined"，什么都渲染不出来。
 * 于是每改一次样式都要重新编译整个 Rust 外壳（几分钟），而样式迭代本该是秒级的。
 *
 * 这里用一份**合成的快照**顶替后端：页面、布局、样式、交互都能照常渲染，
 * 只有真正读写系统的那几个动作是假的（返回快照本身）。
 *
 * # 怎么用
 *
 * ```bash
 * cd apps/ui && npm run dev
 * # 浏览器打开 http://localhost:5173/?preview=1
 * ```
 *
 * 安全性：`preview.ts` 只在 **dev 构建**且 URL 带 `?preview=1` 时才会被动态加载；
 * 生产打包（`npm run build`）里 `import.meta.env.DEV` 恒为 false，这段代码不会执行，
 * 也不会被打进产物（动态 import 的分支被静态求值后摇掉）。
 *
 * 快照的**字段名**由 `apps/desktop/tests/type_contract.rs` 保证与 Rust 一致；
 * 这里的取值只需"看起来像真的"，不必与任何真实状态对应。
 */

import type { AppSnapshot, LogEntry, ProbeResult } from "./types";

const now = Math.floor(Date.now() / 1000);

/** 几个节点，覆盖「快 / 慢 / 连不上」三种呈现，方便检查状态色与文案。 */
const NODES = [
  {
    id: "n-hk-1",
    name: "香港 · REALITY 01",
    address: "45.207.197.185",
    port: 443,
    protocol: { kind: "vless", uuid: "2fa6a093-b4c7-4034-8adf-5f832b29c029", flow: "xtls-rprx-vision", encryption: "none" },
    transport: { kind: "tcp" },
    tls: {
      enabled: true,
      server_name: "www.amazon.com",
      allow_insecure: false,
      alpn: [],
      fingerprint: "chrome",
      reality: { public_key: "OBLL4KvW5Eay3H4UXlbXNlIWLyg4KaUN9vykf77c5hM", short_id: "b0c387ac7c855dc4", spider_x: "/" },
    },
    mux: null,
    source: { kind: "manual" },
    tags: [],
    raw_uri: null,
  },
  {
    id: "n-jp-2",
    name: "日本 · 大阪 BGP",
    address: "203.0.113.24",
    port: 8443,
    protocol: { kind: "vless", uuid: "11111111-2222-3333-4444-555555555555", flow: "", encryption: "none" },
    transport: { kind: "web_socket", path: "/ws", host: "cdn.example.com" },
    tls: { enabled: true, server_name: "cdn.example.com", allow_insecure: false, alpn: ["h2"], fingerprint: "chrome", reality: null },
    mux: null,
    source: { kind: "subscription", id: "sub-1" },
    tags: [],
    raw_uri: null,
  },
  {
    id: "n-us-3",
    name: "美国 · 洛杉矶 CN2",
    address: "198.51.100.7",
    port: 2053,
    protocol: { kind: "trojan", password: "redacted" },
    transport: { kind: "grpc", service_name: "grpc", multi_mode: false },
    tls: { enabled: true, server_name: "us.example.net", allow_insecure: false, alpn: [], fingerprint: "chrome", reality: null },
    mux: null,
    source: { kind: "subscription", id: "sub-1" },
    tags: [],
    raw_uri: null,
  },
  {
    id: "n-sg-4",
    name: "新加坡 · 直连",
    address: "192.0.2.55",
    port: 443,
    protocol: { kind: "trojan", password: "redacted" },
    transport: { kind: "tcp" },
    tls: { enabled: true, server_name: "sg.example.net", allow_insecure: false, alpn: [], fingerprint: "chrome", reality: null },
    mux: null,
    source: { kind: "subscription", id: "sub-2" },
    tags: [],
    raw_uri: null,
  },
] as unknown as AppSnapshot["nodes"];

function probe(nodeId: string, rtt: number | null, ok: boolean, err: string | null): ProbeResult {
  return {
    node_id: nodeId,
    node_name: nodeId,
    server_rtt_ms: rtt,
    available: ok,
    through_node_ms: ok ? (rtt ?? 0) + 130 : null,
    http_status: ok ? 204 : null,
    error: err,
    tested_at: now,
  };
}

/**
 * 按场景改造快照，方便用截图检查各状态的呈现。
 *
 * `?preview=1&state=connected|uncommitted|disconnected|no-core|stale|notice`
 * 默认 `uncommitted`（两阶段启动的中间态，最值得看的一种）。
 */
export function scenarioSnapshot(): AppSnapshot {
  const base = structuredClone(BASE_SNAPSHOT);
  const state = new URLSearchParams(location.search).get("state") ?? "uncommitted";
  switch (state) {
    case "connected":
      base.settings.mode = "tun";
      base.settings.routing_preset = "bypass_mainland";
      base.runtime.routes_committed = true;
      base.runtime.tun_interface = "utun3";
      base.notice = null;
      base.helper = { ...base.helper, socket_present: true, reachable: true, version: "0.8.0", protocol: 1, tun_active: true, state: "ready" } as AppSnapshot["helper"];
      break;
    case "disconnected":
      base.settings.mode = "tun";
      base.runtime = { ...base.runtime, running: false, pid: null, started_at_unix: null, tun_interface: null, routes_committed: false, config_path: null, last_good_node: null } as AppSnapshot["runtime"];
      base.traffic = { rx_bytes: 8_412_774_400, tx_bytes: 1_204_887_552, rx_rate: 0, tx_rate: 0 };
      base.notice = null;
      break;
    case "no-core":
      base.core = { ...base.core, path: null, version: null, supports_native_tun: false, error: "找不到 Xray 核心可执行文件" } as AppSnapshot["core"];
      base.notice = null;
      break;
    case "stale":
      base.helper = { ...base.helper, stale_session: "sess-7f3a91", state: "not_running" } as AppSnapshot["helper"];
      break;
    case "notice":
      // 故意堆多条，检查「只显示最急一条 + 还有 N 条」
      base.core = { ...base.core, supports_native_tun: false } as AppSnapshot["core"];
      base.runtime.last_error = "上次启动失败：端口 10808 被占用";
      break;
    case "uncommitted":
    default:
      break;
  }
  return base;
}

const BASE_SNAPSHOT: AppSnapshot = {
  settings: {
    mode: "system_proxy",
    socks_port: 10808,
    http_port: 10809,
    allow_lan: false,
    selected_node: "n-hk-1",
    routing_preset: "bypass_mainland",
    custom_rules: [],
    tun: {
      capture_ipv6: false,
      mtu: 1500,
      bypass_hosts: [],
      install_default_routes: true,
      dns_handling: "split_by_rule",
      ipv6_mode: "passthrough",
      datapath: "xray_native_tun",
      fd_ownership: "helper_holds",
      fake_dns: { enabled: true, cidr: "198.18.0.0/15", ttl: 60 },
    } as unknown as AppSnapshot["settings"]["tun"],
    dns: {
      direct_servers: ["223.5.5.5", "119.29.29.29"],
      remote_servers: ["https://1.1.1.1/dns-query"],
      hosts: [],
      fake_dns: true,
      query_strategy: "use_ip",
      fallback_servers: [],
    } as unknown as AppSnapshot["settings"]["dns"],
    fakedns: { enabled: true, cidr: "198.18.0.0/15", ttl: 60 },
    core_path: null,
    launch_at_login: true,
    log_level: "info",
    restore_system_proxy_on_exit: true,
    show_speed_in_title: false,
  } as unknown as AppSnapshot["settings"],
  subscriptions: [
    {
      id: "sub-1",
      name: "主订阅 · 机场 A",
      url: "https://sub.example.com/api/v1/client/subscribe?token=redacted",
      node_count: 3,
      last_updated: now - 1800,
      last_error: null,
      // 形状必须与后端一致：`SubscriptionUsage { upload, download, total, expire }`
      // ——注意到期字段叫 `expire`（不是 `expire_at`），早先这里写错，
      // 于是用量条在预览里根本不渲染，把「mock 写错」伪装成「界面没实现」。
      usage: {
        upload: 12_884_901_888,
        download: 88_312_678_400,
        total: 500_000_000_000,
        expire: now + 86_400 * 47,
      },
    } as unknown as AppSnapshot["subscriptions"][number],
    {
      id: "sub-2",
      name: "备用 · 机场 B",
      url: "https://sub2.example.net/link/redacted",
      node_count: 1,
      last_updated: now - 86_400 * 3,
      last_error: "HTTP 403：订阅 token 可能已过期",
      usage: null,
    } as unknown as AppSnapshot["subscriptions"][number],
    {
      // 第三种形状：**不限量（total == 0）但有有效期**。
      // 后端把 total == 0 定义为「不限量」，这种订阅没有用量比例可画，
      // 但到期时间仍然必须显示（曾经被整块藏掉，是个回归）。
      id: "sub-3",
      name: "不限量 · 机场 C",
      url: "https://sub3.example.org/link/redacted",
      node_count: 0,
      last_updated: now - 600,
      last_error: null,
      usage: { upload: 1_073_741_824, download: 5_368_709_120, total: 0, expire: now + 86_400 * 120 },
    } as unknown as AppSnapshot["subscriptions"][number],
  ],
  nodes: NODES,
  runtime: {
    running: true,
    pid: 36019,
    started_at_unix: now - 742,
    config_path: "/Users/me/Library/Application Support/com.xraytun.desktop/runtime/config.json",
    tun_session: null,
    tun_interface: null,
    routes_committed: false,
    last_error: null,
    last_good_node: "n-hk-1",
  } as unknown as AppSnapshot["runtime"],
  latency: {
    "n-hk-1": probe("n-hk-1", 53, true, null),
    "n-jp-2": probe("n-jp-2", 88, true, null),
    "n-us-3": probe("n-us-3", 167, true, null),
    "n-sg-4": probe("n-sg-4", 61, false, "探针失败: 等待首字节超时（总预算 5s）"),
  },
  traffic: { rx_bytes: 8_412_774_400, tx_bytes: 1_204_887_552, rx_rate: 1_886_464, tx_rate: 235_520 },
  notice: "helper 未安装：TUN 模式需要它。系统代理模式不受影响。",
  helper: {
    socket_present: false,
    reachable: false,
    version: null,
    protocol: null,
    tun_active: false,
    stale_session: null,
    needs_approval: false,
    error: null,
    state: "not_installed",
  } as unknown as AppSnapshot["helper"],
  core: {
    path: "/Applications/XrayTun.app/Contents/Resources/xray",
    version: "26.9.9",
    error: null,
    supports_native_tun: true,
    min_native_tun_version: "26.1.31",
  } as unknown as AppSnapshot["core"],
  login_item: { status: "enabled", detail: "已开启，登录时自动启动", needs_approval: false },
  update: {
    core_version: "26.9.9",
    core_managed: false,
    core_managed_version: null,
    geo_tag: "v26.9.9",
    geo_installed_at: now - 86_400 * 5,
    latest_core: null,
    latest_geo: null,
    latest_app: null,
    checked_at: now - 300,
    check_error: null,
    progress: null,
  } as unknown as AppSnapshot["update"],
  dns: {
    probes: [
      { server: "223.5.5.5", kind: "direct", latency_ms: 12, answered: true, error: null },
      { server: "119.29.29.29", kind: "direct", latency_ms: 18, answered: true, error: null },
      { server: "https://1.1.1.1/dns-query", kind: "foreign", latency_ms: 148, answered: true, error: null },
    ] as unknown as AppSnapshot["dns"]["probes"],
    chosen: "223.5.5.5",
    chosen_foreign: "https://1.1.1.1/dns-query",
    probed_at: now - 600,
    error: null,
    foreign_error: null,
  } as unknown as AppSnapshot["dns"],
  app_version: "0.8.0",
};


/** 预览用的拓扑：形状与真实配置一致（8 条规则、4 个入口、5 个出口）。 */
const MOCK_TOPOLOGY = {
  inbound: [
    { tag: "tun", protocol: "tun", port: null, uplink_bytes: 1_204_887_552, downlink_bytes: 8_412_774_400 },
    { tag: "socks", protocol: "socks", port: 10808, uplink_bytes: 42_000_000, downlink_bytes: 310_000_000 },
    { tag: "http", protocol: "http", port: 10809, uplink_bytes: 0, downlink_bytes: 0 },
    { tag: "api", protocol: "dokodemo-door", port: 10085, uplink_bytes: 0, downlink_bytes: 0 },
  ],
  rule: [
    { index: 0, tag: "internal-dns-hijack", outbound: "dns-out", conditions: ["IP 198.18.0.2", "端口 53"] },
    { index: 1, tag: "internal-api", outbound: "api", conditions: ["入站 api"] },
    { index: 2, tag: "preset-private", outbound: "direct", conditions: ["域名 geosite:private", "IP geoip:private"] },
    { index: 3, tag: "preset-ads", outbound: "block", conditions: ["域名 geosite:category-ads-all"] },
    { index: 4, tag: "preset-proxy-google", outbound: "node-n1d232c6b8c7a5004", conditions: ["域名 geosite:google"] },
    { index: 5, tag: "preset-cn-domain", outbound: "direct", conditions: ["域名 geosite:cn"] },
    { index: 6, tag: "preset-cn-ip", outbound: "direct", conditions: ["IP geoip:cn"] },
    { index: 7, tag: "internal-fallback", outbound: "node-n1d232c6b8c7a5004", conditions: ["网络 tcp,udp"] },
  ],
  outbound: [
    { tag: "node-n1d232c6b8c7a5004", protocol: "vless", kind: "node", uplink_bytes: 1_246_000_000, downlink_bytes: 8_600_000_000 },
    { tag: "direct", protocol: "freedom", kind: "direct", uplink_bytes: 900_000, downlink_bytes: 120_000_000 },
    { tag: "block", protocol: "blackhole", kind: "block", uplink_bytes: 12_000, downlink_bytes: 0 },
    { tag: "dns-out", protocol: "dns", kind: "dns", uplink_bytes: 300_000, downlink_bytes: 300_000 },
    { tag: "api", protocol: "freedom", kind: "internal", uplink_bytes: 4_000, downlink_bytes: 8_000 },
  ],
  traffic_error: null as string | null,
  traffic_ok: true,
  counter_resets: 0,
  geo_available: true,
};

/**
 * 预览用的拓扑场景。
 *
 * `?preview=1&traffic=unavailable` 模拟「这次没查到流量」。后端契约（task-5）
 * 是：此时所有 `*_bytes` 填**占位 0**、`traffic_ok=false`、`traffic_error`
 * 给出原因，且恒有 `traffic_ok === (traffic_error === null)`。
 *
 * 为什么要有这个开关：没有它时，「查不到流量」这条路径在预览里永远看不到，
 * 前端按 `traffic_ok` 渲染的分支就没法截图、没法回归 —— 上一版正是这么漏的。
 */
function topologyScenario(): typeof MOCK_TOPOLOGY {
  const unavailable = new URLSearchParams(location.search).get("traffic") === "unavailable";
  if (!unavailable) return MOCK_TOPOLOGY;
  return {
    ...MOCK_TOPOLOGY,
    traffic_error: "核心未运行：读不到 StatsService，本次流量不可用",
    traffic_ok: false,
    counter_resets: 0,
    // 与后端一致：不可用时字节是**占位 0**，不是真实读数。
    inbound: MOCK_TOPOLOGY.inbound.map((x) => ({ ...x, uplink_bytes: 0, downlink_bytes: 0 })),
    outbound: MOCK_TOPOLOGY.outbound.map((x) => ({ ...x, uplink_bytes: 0, downlink_bytes: 0 })),
  };
}

/** 造一批日志：前 120 条用来撑出可滚动区域，末尾几条覆盖 info/warn/error 与多行文本。 */
export const MOCK_LOGS: LogEntry[] = [
  ...Array.from({ length: 120 }, (_, i) => ({
    ts_unix: now - 900 + i,
    source: i % 3 === 0 ? "core" : "app",
    level: "info",
    message: `预热日志 #${i}：填充滚动区域，用于验证「跟随」开关是否真的生效`,
  })),

  { ts_unix: now - 740, source: "app", level: "info", message: "正在切换到「香港 · REALITY 01」，需要重建隧道（几秒）" },
  { ts_unix: now - 736, source: "core", level: "info", message: "Xray 26.9.9 started" },
  { ts_unix: now - 735, source: "app", level: "info", message: "连通性检查通过：经节点 189ms（HTTP 204）" },
  { ts_unix: now - 420, source: "core", level: "warn", message: "failed to dial 198.51.100.7:2053: connection reset by peer" },
  {
    ts_unix: now - 60,
    source: "core",
    level: "error",
    message: "启动失败:\n    \"port\": 10808\n    已被占用（另一个代理工具在跑？）",
  },
];

// ---------------------------------------------------------------------------
// 拓扑自检探针（**只在 `?preview=1` 下存在**）
// ---------------------------------------------------------------------------
//
// # 为什么需要它
//
// 拓扑动画的「数据乱跳」以前只能靠临时 CDP 脚本排查：每个人写一份、
// 每次量的口径还不一样（有人量屏幕位移，有人量进度增量），数字没法对比。
// 这里把口径固化下来，测试与排查都用**同一条命令**量。
//
// # 关键取舍
//
// 探针**只读 DOM**，不碰 `Topology.tsx` 的内部状态：
//   - 位置：`.flow__truck` 元素的 `transform="translate(x y)"`；
//   - 进度：把该点投到同一条路线的 `.flow__guide` 路径上反推弧长比例
//     （`getPointAtLength` 采样 + 局部细化）。所以 progress 是**独立重建**的，
//     不是从组件里读出来的，能真正校验「车在不在线上」。
//
// # 两条判据（缺一不可）
//
// 1. **几何**（`guide_to_visible`）：`guide` 是车真正走的隐藏路径，`flow__route`
//    才是眼睛能看到的线。这两条**必须重合** —— B1 那次故障就是它们分家了：
//    车有 1/4 的行程飞在空白处，而「车→guide」距离恒为 0，所以只量后者会
//    **报告一切正常**。这里按 1px 采样 guide，量它到所有可见线的最短距离。
// 2. **运动**（`steps`）：相邻帧的位移不应出现孤立尖峰。判据用**速率**
//    （`max/median of step÷帧间隔`）而不是裸步长 —— 动画自己有「一帧最多补 2 帧」
//    的策略，掉一帧时裸步长就是稳态的整 2 倍（实测 19.10 = 9.5×2），
//    裸比值会被抬到 2.1–2.2 而误报；按 dt 归一化后掉帧**不会**抬高判据。
//
// 判据用**比值**而不是固定像素阈值：路径长度会随卡片宽度变化，固定阈值会误判。
// 固定阈值（例如「>50px 就算跳」）在长路径上会漏报、短路径上会误报。
// # 怎么用
//
// ```js
// window.__topologyProbe()               // 取当前快照 + 最近 N 帧步长统计
// window.__topologyProbe({ reset: true }) // 先清空帧缓冲再取（测试前调用）
// ```
//
// 帧的采集用 `MutationObserver` 盯 `.flow__truck` 的 `transform`，而不是自己
// 再跑一个 rAF：动画每帧写一次 transform，观察到的就是**动画自己的帧**，
// 采样间隔与它的 `dt` 同源，不会因为两个 rAF 回调的先后相位差而虚增步长。
// （自跑 rAF 的版本在 CPU 被 `cargo test` 抢走时会把 ratio 从 1.36 抬到 2.64，
//  全是测量误差。）
//
// 默认保留 240 帧（约 4 秒 @60fps），可用 `?preview=1&probeFrames=900` 调大窗口。

/** 帧缓冲长度；`?probeFrames=` 只能调大，上限 3600 帧（约 1 分钟）。 */
const PROBE_FRAME_LIMIT = (() => {
  const raw = Number(new URLSearchParams(location.search).get("probeFrames"));
  return Number.isFinite(raw) && raw >= 10 ? Math.min(3600, Math.floor(raw)) : 240;
})();

interface ProbeFrameTruck {
  /** 稳定身份：`data-truck-key`（如 `route#3`，跨刷新不变）。 */
  key: string;
  slot: number;
  route: number;
  x: number;
  y: number;
}

interface ProbeFrame {
  t: number;
  /** 按**身份**（`data-truck-key`）配对的车辆快照。 */
  trucks: ProbeFrameTruck[];
}

export interface TopologyProbeTruck {
  /** 稳定身份（`data-truck-key`）；DOM 上缺失时退化为 `slot:<n>`。 */
  key: string;
  slot: number;
  route: number;
  /** 0..1；从 DOM 上的导引路径反推。定位不到时为 null。 */
  progress: number | null;
  x: number;
  y: number;
  /**
   * 与导引路径的最近距离（px）。正常应 <2px —— 这是采样缓存的**分辨率上限**
   * （约每 2px 一个采样点，分叉处可能选到相邻分支）。明显大于 2px 才说明车脱线。
   */
  off_path: number | null;
  /** 当前填充色，即「车当前走的是哪条分支」。 */
  fill: string | null;
}

/** 判读档位：见 `hint`。 */
export type TopologyProbeVerdict = "ok" | "suspect" | "jump" | "off_line" | "unknown";

/**
 * 「车走的隐藏路径」与「眼睛能看到的线」是否重合。
 *
 * 为什么单独有这一条：`off_path` 量的是「车 → guide」，而车本来就沿 guide 走，
 * 所以它**恒 ≈0，对 B1 那类故障完全瞎**（实测 B1 回退版上 `off_path` 报
 * `max 0.38px / 0 辆 >1.5px`，而真相是 31% 行程离线、最大 32px）。
 */
export interface TopologyProbeGuideToVisible {
  guides: number;
  visible_paths: number;
  /** 可见线按 2px 采样成的折线段数（与仓库回归测试同口径）。 */
  visible_segments: number;
  /** guide 上 1px 一个采样点，共采了多少点。 */
  samples: number;
  /** 离最近可见线 >1.5px 的行程占比，**百分比**（0–100）。阈值 <2。 */
  off_line_frac: number;
  /** 最大偏离（px）。阈值 <3。 */
  off_line_max: number;
  /** `off_line_frac < 2 && off_line_max < 3`。 */
  ok: boolean;
}

export interface TopologyProbeSteps {
  /** 参与统计的相邻帧步数。 */
  samples: number;
  max: number;
  median: number;
  p95: number;
  /**
   * 裸步长的最大/中位之比。**保留作参考，不作判据** —— 见下面那条。
   *
   * 为什么不用它判：动画自己有「一帧最多补 2 帧」的策略
   * （`dt > 1/30 → dt = 1/30`），主线程掉一帧时裸步长就是稳态的整 2 倍
   * （实测 19.10px = 正常 9.5px 的 2.0 倍），裸比值会被抬到 2.1–2.2 而误报。
   */
  ratio_max_median: number | null;
  /**
   * **判据**：把每步按帧间隔归一化成速度（px/s）后再取最大/中位。
   *
   * 掉帧时步长变大、间隔也变大，两者相抵，所以这条不会因为「设计内的补帧」
   * 误报（实测掉帧场景 19.10px/33ms ≈ 正常 9.5px/16.7ms）。而注入瞬移是
   * 同一帧内瞬移几百像素，速度会飙到 50 倍 —— 实测 38.7（裸比值）/ 同量级的
   * 速率比，余量充足。
   */
  rate_max_median: number | null;
}

export interface TopologyProbeFrameGaps {
  max_ms: number;
  median_ms: number;
  p95_ms: number;
}

export interface TopologyProbeResult {
  /**
   * 只有**确定性的故障**才为 false：位置跳变（`jump`）、
   * guide 与可见线分家（`off_line`）、或没有样本（`unknown`）。
   * `suspect`（速率比落在 2–3）**不算失败** —— 见 `steps.rate_max_median`。
   */
  ok: boolean;
  /** 判读结果：`ok` / `suspect` / `jump` / `off_line` / `unknown`。 */
  verdict: TopologyProbeVerdict;
  hint: string;
  /** 帧缓冲里的帧数（含空帧）与时间跨度。`frames` 已合并同帧双写，`batches` 是原始批数。 */
  frames: number;
  batches: number;
  window_ms: number;
  /** 配对同一辆车用的身份来源：`data-truck-key`（正常）或 `data-slot`（退化）。 */
  identity: string;
  /** 缓冲窗口内货车数量发生变化的次数。 */
  truck_count_changes: number;
  /** 同一辆车（按身份）换了路线的次数。 */
  route_changes: number;
  trucks: TopologyProbeTruck[];
  steps: TopologyProbeSteps;
  /** rAF 帧间隔；用来判断尖峰是不是「环境卡顿」造成的。 */
  frame_gaps: TopologyProbeFrameGaps;
  /** 隐藏导引路径是否与可见线重合（B1 类故障的判据）。 */
  guide_to_visible: TopologyProbeGuideToVisible;
  /** 按**身份**拆开的步长（按 max 从大到小排），用来回答「跳的是哪辆车」。 */
  per_truck: { key: string; slot: number; route: number; samples: number; max: number; median: number }[];
}

/** 解析 `transform="translate(x y)"`（也接受逗号）。 */
function parseTranslate(transform: string | null): { x: number; y: number } | null {
  if (!transform) return null;
  const m = /translate\(\s*(-?[\d.]+)\s*[,\s]\s*(-?[\d.]+)\s*\)/.exec(transform);
  if (!m || m[1] === undefined || m[2] === undefined) return null;
  return { x: Number(m[1]), y: Number(m[2]) };
}

/** 可见线采样出来的一条线段，带包围盒用于快速排除。 */
interface VisibleSeg {
  ax: number;
  ay: number;
  bx: number;
  by: number;
  minx: number;
  miny: number;
  maxx: number;
  maxy: number;
}

/** 点到线段的距离。 */
function distToSeg(seg: VisibleSeg, x: number, y: number): number {
  const vx = seg.bx - seg.ax;
  const vy = seg.by - seg.ay;
  const l2 = vx * vx + vy * vy;
  let t = l2 > 0 ? ((x - seg.ax) * vx + (y - seg.ay) * vy) / l2 : 0;
  if (t < 0) t = 0;
  else if (t > 1) t = 1;
  return Math.hypot(x - (seg.ax + t * vx), y - (seg.ay + t * vy));
}

/**
 * 量「隐藏 guide」与「可见线」的距离 —— B1 类故障的**唯一**判据。
 *
 * 口径与仓库里的回归测试 `topologyAnimation.test.ts`（“guide 路径必须等于可见线的并集”）
 * 完全一致，所以两边的数字可以直接对比：
 *   - guide 每 **1px** 采一个点；
 *   - 可见线（`path.flow__route`）每 **2px** 采成折线；
 *   - 点到**所有**折线段的**最短**距离；
 *   - `> 1.5px` 记一次离线，占比按采样点数（= 弧长占比）；
 *   - 阈值：占比 `< 2%` 且最大偏离 `< 3px`。
 *
 * 实测（test-verifier，真浏览器）：修复版 `0.00% / 0.00px`；
 * B1 回退版 `31.35% / 32.59px`。差距极大，阈值余量充足。
 *
 * # 为什么要网格
 *
 * 朴素做法是「每个 guide 采样点 × 每条可见线段」：实测 16222 采样点 × 8096 条线段
 * = 1.3 亿次距离计算，**首次调用要 10 秒**（会把渲染主线程卡住、CDP 都能拖超时）。
 * 改成按 24px 的网格桶存线段 + 由内向外逐环查找：在线样本第一环就命中，
 * 实测降到毫秒级。
 */
function measureGuideToVisible(
  guides: SVGPathElement[],
  visible: SVGPathElement[],
): TopologyProbeGuideToVisible {
  const segs: VisibleSeg[] = [];
  for (const p of visible) {
    let L: number;
    try {
      L = p.getTotalLength();
    } catch {
      continue;
    }
    if (!(L > 0)) continue;
    let prev: { x: number; y: number } | null = null;
    for (let l = 0; l <= L; l += 2) {
      const q = p.getPointAtLength(l);
      if (prev) {
        const ax = prev.x;
        const ay = prev.y;
        const bx = q.x;
        const by = q.y;
        segs.push({
          ax,
          ay,
          bx,
          by,
          minx: Math.min(ax, bx),
          miny: Math.min(ay, by),
          maxx: Math.max(ax, bx),
          maxy: Math.max(ay, by),
        });
      }
      prev = { x: q.x, y: q.y };
    }
  }

  const CELL = 24;
  const grid = new Map<string, VisibleSeg[]>();
  for (const s of segs) {
    for (let cx = Math.floor(s.minx / CELL); cx <= Math.floor(s.maxx / CELL); cx++) {
      for (let cy = Math.floor(s.miny / CELL); cy <= Math.floor(s.maxy / CELL); cy++) {
        const k = `${cx},${cy}`;
        const bucket = grid.get(k);
        if (bucket) bucket.push(s);
        else grid.set(k, [s]);
      }
    }
  }

  let samples = 0;
  let off = 0;
  let worst = 0;
  for (const g of guides) {
    let L: number;
    try {
      L = g.getTotalLength();
    } catch {
      continue;
    }
    if (!(L > 0)) continue;
    for (let l = 0; l <= L; l += 1) {
      const q = g.getPointAtLength(l);
      const cx = Math.floor(q.x / CELL);
      const cy = Math.floor(q.y / CELL);
      let best = Infinity;
      // 由内向外逐环查找；一旦「已找到的最优」近于下一个环的内侧边界，
      // 就不可能有更近的线段了（环上任何点距离 ≥ r*CELL），可以停。
      for (let r = 0; r <= 12; r++) {
        for (let ix = cx - r; ix <= cx + r; ix++) {
          for (let iy = cy - r; iy <= cy + r; iy++) {
            // 只扫这一环（Chebyshev 距离恰为 r 的格子）
            if (r > 0 && Math.max(Math.abs(ix - cx), Math.abs(iy - cy)) !== r) continue;
            const bucket = grid.get(`${ix},${iy}`);
            if (!bucket) continue;
            for (const s of bucket) {
              const dx = Math.max(s.minx - q.x, 0, q.x - s.maxx);
              const dy = Math.max(s.miny - q.y, 0, q.y - s.maxy);
              if (dx * dx + dy * dy >= best * best) continue;
              const d = distToSeg(s, q.x, q.y);
              if (d < best) best = d;
            }
          }
        }
        if (best <= r * CELL) break;
      }
      samples += 1;
      if (best > 1.5) off += 1;
      if (best > worst) worst = best;
    }
  }

  const fracPct = samples > 0 ? (off / samples) * 100 : 0;
  return {
    guides: guides.length,
    visible_paths: visible.length,
    visible_segments: segs.length,
    samples,
    off_line_frac: Number(fracPct.toFixed(2)),
    off_line_max: Number((worst === Infinity ? 999 : worst).toFixed(2)),
    ok: samples > 0 && fracPct < 2 && worst < 3,
  };
}

/**
 * 几何签名：guide 与可见线的 `d` 拼起来。
 *
 * 这条测量要跑几十万次点到线段距离（实测首次约 0.2–0.5s）。几何只会在
 * 「重新测量 / 窗口变化 / 数据刷新改了卡片宽度」时变，所以按签名缓存 ——
 * 同一几何下重复调 `__topologyProbe()` 是零成本的，测试循环里很关键。
 */
function geometrySignature(guides: SVGPathElement[], visible: SVGPathElement[]): string {
  const ds = (els: SVGPathElement[]) => els.map((p) => p.getAttribute("d") ?? "").join("|");
  return `${guides.length}:${visible.length}#${ds(guides)}##${ds(visible)}`;
}

/**
 * 一条导引路径的采样缓存。
 *
 * 为什么要有缓存：`getPointAtLength` 在这几条路径上约 **0.28ms/次**（实测），
 * 若每辆车都从头粗采样 800 点，14 辆车就要 22000 次 ≈ 6.3 秒，会把渲染主线程
 * 卡住到 CDP 连接超时。改成「每条路径采样一次（约每 2px 一个点），扫数组找最近点，
 * 再局部细化」：3 条路径 + 14 辆车的细化 ≈ 2000 次 ≈ 0.6 秒。
 */
interface GuideCache {
  total: number;
  ls: number[];
  xs: number[];
  ys: number[];
  step: number;
}

function sampleGuide(path: SVGPathElement, total: number): GuideCache {
  // 约每 2px 一个采样点（上限 600）：分叉点附近两条分支可能只差几像素，
  // 采样太稀时最优点会落到另一条分支上，`off_path` 会虚报（实测 5-7px）。
  const n = Math.min(600, Math.max(100, Math.ceil(total / 2)));
  const ls: number[] = [];
  const xs: number[] = [];
  const ys: number[] = [];
  for (let i = 0; i <= n; i++) {
    const l = (i / n) * total;
    const p = path.getPointAtLength(l);
    ls.push(l);
    xs.push(p.x);
    ys.push(p.y);
  }
  return { total, ls, xs, ys, step: total / n };
}

/** 在缓存里找最近点，再在它附近做局部细化（直线搜索 + 折半）。 */
function nearestOnPath(path: SVGPathElement, cache: GuideCache, x: number, y: number): { l: number; d2: number } {
  let bestL = 0;
  let bestD = Infinity;
  for (let i = 0; i < cache.ls.length; i++) {
    const dx = (cache.xs[i] ?? 0) - x;
    const dy = (cache.ys[i] ?? 0) - y;
    const d2 = dx * dx + dy * dy;
    if (d2 < bestD) {
      bestD = d2;
      bestL = cache.ls[i] ?? 0;
    }
  }
  let step = cache.step;
  for (let iter = 0; iter < 20 && step > 0.02; iter++) {
    for (const cand of [bestL - step, bestL + step]) {
      const l = Math.min(cache.total, Math.max(0, cand));
      const p = path.getPointAtLength(l);
      const d2 = (p.x - x) ** 2 + (p.y - y) ** 2;
      if (d2 < bestD) {
        bestD = d2;
        bestL = l;
      }
    }
    step /= 2;
  }
  return { l: bestL, d2: bestD };
}

function median(xs: number[]): number {
  if (xs.length === 0) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const mid = Math.floor(s.length / 2);
  if (s.length % 2 === 1) return s[mid] ?? 0;
  return ((s[mid - 1] ?? 0) + (s[mid] ?? 0)) / 2;
}

function percentile(xs: number[], q: number): number {
  if (xs.length === 0) return 0;
  const s = [...xs].sort((a, b) => a - b);
  const idx = Math.min(s.length - 1, Math.max(0, Math.ceil(q * s.length) - 1));
  return s[idx] ?? 0;
}

/**
 * 把「同一帧里的多次写入」合并成一次。
 *
 * 为什么需要：dev 下 React 严格模式会把动画 effect 挂两次，于是同一帧里有
 * 两个 rAF 回调各写一次 `transform`；而 MutationObserver 的回调是 microtask，
 * 两个回调之间会各交一批记录 —— 表现为「间隔 0.2ms 的两帧」。
 * 不合并的话 `步长/间隔` 会被这种同帧双写放大到 30×（实测），
 * 看起来像故障。合并保留**后写**的那次（后写的才是屏幕上看到的）。
 *
 * 8ms（约半帧）的阈值远小于一帧（16.7ms），所以不会把真实的两帧并掉。
 */
function mergeSameFrame(frames: ProbeFrame[], minGapMs = 8): ProbeFrame[] {
  const out: ProbeFrame[] = [];
  for (const f of frames) {
    const last = out[out.length - 1];
    if (last && f.t - last.t < minGapMs) {
      out[out.length - 1] = f;
      continue;
    }
    out.push(f);
  }
  return out;
}

export function installTopologyProbe(): () => void {
  const frames: ProbeFrame[] = [];
  /** 上一批 transform 写入的时间；间隔过大说明拓扑卸载过，中间要插一个空帧。 */
  let lastBatchAt = 0;
  /** 超过这个间隔没有写入，就认为「中间什么都没发生」，不把两侧配成一步。 */
  const STALE_MS = 300;

  const observer = new MutationObserver((records) => {
    const byKey = new Map<string, ProbeFrameTruck>();
    for (const r of records) {
      if (r.attributeName !== "transform") continue;
      const el = r.target as SVGGElement;
      if (!el.classList?.contains("flow__truck")) continue;
      const p = parseTranslate(el.getAttribute("transform"));
      if (!p) continue;
      const slot = Number(el.dataset.slot ?? 0);
      // **按 `data-truck-key` 配对**，不按 slot：修复后车辆数量会随流量变化
      // （预告是 14 辆，跨档位时会增减），按下标比「同一辆车」会整体错位，
      // 报出假跳变。缺 key 时才退化为 `slot:<n>`。
      const key = el.dataset.truckKey ?? `slot:${slot}`;
      byKey.set(key, { key, slot, route: Number(el.dataset.route ?? 0), x: p.x, y: p.y });
    }
    if (byKey.size === 0) return;
    const t = performance.now();
    // 空帧让「卸载 → 挂载」之间的两帧不会被配成一步，
    // 否则一次页面切换会被误算成一个巨大的位移尖峰。
    if (lastBatchAt > 0 && t - lastBatchAt > STALE_MS) frames.push({ t, trucks: [] });
    lastBatchAt = t;
    frames.push({ t, trucks: [...byKey.values()] });
    while (frames.length > PROBE_FRAME_LIMIT) frames.shift();
  });
  // 挂在 documentElement 上（而不是 `.flow`）是因为 SVG 会随页面切换重建，
  // 观察根节点就不必在每次挂载后重新绑定。
  observer.observe(document.documentElement, { subtree: true, attributes: true, attributeFilter: ["transform"] });

  /** `guide_to_visible` 的缓存：几何没变就不重复跑那几十万次距离计算。 */
  let geoCache: { sig: string; value: TopologyProbeGuideToVisible } | null = null;

  const currentTrucks = (): TopologyProbeTruck[] => {
    const guides = [...document.querySelectorAll<SVGPathElement>(".flow__guide")];
    // 每条路径只采样一次（见 GuideCache 的说明）
    const caches = guides.map((g) => {
      try {
        const t = g.getTotalLength();
        return t > 0 ? sampleGuide(g, t) : null;
      } catch {
        return null;
      }
    });
    const out: TopologyProbeTruck[] = [];
    document.querySelectorAll<SVGGElement>(".flow__truck").forEach((el, i) => {
      const p = parseTranslate(el.getAttribute("transform"));
      if (!p) return;
      const route = Number(el.dataset.route ?? 0);
      const guide = guides[route];
      const cache = caches[route] ?? null;
      let progress: number | null = null;
      let offPath: number | null = null;
      if (guide && cache) {
        const hit = nearestOnPath(guide, cache, p.x, p.y);
        progress = hit.l / cache.total;
        offPath = Math.sqrt(hit.d2);
      }
      out.push({
        key: el.dataset.truckKey ?? `slot:${Number(el.dataset.slot ?? i)}`,
        slot: Number(el.dataset.slot ?? i),
        route,
        progress,
        x: p.x,
        y: p.y,
        off_path: offPath,
        fill: el.querySelector("rect")?.getAttribute("fill") ?? null,
      });
    });
    return out;
  };

  const probe = (opts?: { reset?: boolean } | boolean): TopologyProbeResult => {
    const reset = opts === true || (typeof opts === "object" && opts !== null && opts.reset === true);
    if (reset) {
      frames.length = 0;
      lastBatchAt = 0;
    }
    // 统计口径统一用「合并后的帧」，见 mergeSameFrame 的说明。
    const merged = mergeSameFrame(frames);

    const steps: number[] = [];
    const gaps: number[] = [];
    const rates: number[] = []; // 步长 ÷ 帧间隔（px/s）——判据用这条
    const byKey = new Map<string, { slot: number; route: number; xs: number[] }>();
    let truckCountChanges = 0;
    let routeChanges = 0;
    for (let i = 1; i < merged.length; i++) {
      const a = merged[i - 1];
      const b = merged[i];
      if (!a || !b) continue;
      const gap = b.t - a.t;
      if (gap > 0) gaps.push(gap);
      if (a.trucks.length !== b.trucks.length) truckCountChanges++;
      // **按身份配对**：只有两帧都出现过的 key 才算「同一辆车走了一步」。
      // 新车（本帧才出现）没有前一帧位置，不构成一步，也不算跳变。
      const prev = new Map(a.trucks.map((x) => [x.key, x]));
      for (const pb of b.trucks) {
        const pa = prev.get(pb.key);
        if (!pa) continue;
        if (pa.route !== pb.route) routeChanges++;
        const d = Math.hypot(pb.x - pa.x, pb.y - pa.y);
        steps.push(d);
        // 只取正常帧长（≥8ms）算速率：更小的间隔是「同一帧被拆成多批」的产物
        // （见 mergeSameFrame），拿它当分母会把速率放大几十倍，是纯噪声。
        if (gap >= 8) rates.push((d / gap) * 1000);
        const rec = byKey.get(pb.key);
        if (rec) rec.xs.push(d);
        else byKey.set(pb.key, { slot: pb.slot, route: pb.route, xs: [d] });
      }
    }

    const max = steps.length > 0 ? Math.max(...steps) : 0;
    const med = median(steps);
    const ratio = steps.length >= 2 && med > 0 ? max / med : null;
    // 速率（px/s）= 步长 ÷ 帧间隔。**判据用这条**：掉帧时步长和间隔一起变大，
    // 相抵之后不会像裸步长那样被「设计内的补帧」抬到 2 倍。见 rate_max_median 的说明。
    const rateMax = rates.length > 0 ? Math.max(...rates) : 0;
    const rateMed = median(rates);
    const rateRatio = rates.length >= 2 && rateMed > 0 ? rateMax / rateMed : null;
    const first = merged[0];
    const last = merged[merged.length - 1];
    const gapsMax = gaps.length > 0 ? Math.max(...gaps) : 0;

    // 快照只算一次：`currentTrucks()` 每次要跑上千次 getPointAtLength。
    const snapshot = currentTrucks();

    // 几何判据（B1 类）：guide 与可见线是否重合。按几何签名缓存，重复调用零成本。
    const guides = [...document.querySelectorAll<SVGPathElement>(".flow__guide")];
    const visiblePaths = [...document.querySelectorAll<SVGPathElement>(".flow__route")];
    const geoKey = geometrySignature(guides, visiblePaths);
    if (!geoCache || geoCache.sig !== geoKey) {
      geoCache = { sig: geoKey, value: measureGuideToVisible(guides, visiblePaths) };
    }
    const guideToVisible = geoCache.value;

    // 判读顺序：**几何硬故障优先**（它不看样本量），再看运动。
    let verdict: TopologyProbeVerdict;
    if (!guideToVisible.ok) verdict = "off_line";
    else if (rateRatio === null) verdict = "unknown";
    else if (rateRatio > 3) verdict = "jump";
    else if (rateRatio > 2) verdict = "suspect";
    else verdict = "ok";

    return {
      // `suspect` 不算失败：它落在「速率比 2–3」这条带里，多半是环境卡顿或补帧，
      // 只有几何分家（off_line）、确定跳变（jump）、无样本（unknown）才是故障。
      ok: verdict === "ok" || verdict === "suspect",
      verdict,
      hint:
        "判据两条：① 几何 guide_to_visible —— 隐藏导引路径与可见线必须重合，" +
        "off_line_frac <2% 且 off_line_max <3px，否则 verdict=off_line；" +
        "② 运动 steps.rate_max_median（步长÷帧间隔 的最大/中位）—— ≤2 正常，2–3 suspect（不算失败），>3 jump。" +
        "不要用裸步长 ratio_max_median 判读：动画有「一帧最多补 2 帧」的策略，" +
        "掉一帧时裸步长恰好是稳态的整 2 倍（实测 19.10 = 9.5×2），会误报。" +
        "配对同一辆车用 data-truck-key（跨刷新稳定），不是 data-slot —— " +
        "车数会随流量变化，按 slot 比会整体错位。",
      frames: merged.length,
      batches: frames.length,
      window_ms: first && last ? Math.max(0, last.t - first.t) : 0,
      // 退化时 key 恒为 `slot:<n>`，所以据此判断身份来源。
      identity: snapshot.length > 0 && snapshot.every((t) => !t.key.startsWith("slot:")) ? "data-truck-key" : "data-slot",
      truck_count_changes: truckCountChanges,
      route_changes: routeChanges,
      trucks: snapshot,
      steps: {
        samples: steps.length,
        max,
        median: med,
        p95: percentile(steps, 0.95),
        ratio_max_median: ratio,
        rate_max_median: rateRatio,
      },
      frame_gaps: {
        max_ms: gapsMax,
        median_ms: median(gaps),
        p95_ms: percentile(gaps, 0.95),
      },
      guide_to_visible: guideToVisible,
      per_truck: [...byKey.entries()]
        .map(([key, v]) => ({
          key,
          slot: v.slot,
          route: v.route,
          samples: v.xs.length,
          max: Math.max(...v.xs),
          median: median(v.xs),
        }))
        // 跳得最厉害的排前面，直接回答「跳的是哪辆车」。
        .sort((a, b) => b.max - a.max),
    };
  };

  (window as unknown as Record<string, unknown>).__topologyProbe = probe;

  return () => {
    observer.disconnect();
    frames.length = 0;
    delete (window as unknown as Record<string, unknown>).__topologyProbe;
  };
}

/** 安装桥接。返回 uninstall，便于热更新时清理。 */
export function installPreviewBridge(): () => void {
  // 拓扑自检探针（`window.__topologyProbe()`）：只读 DOM，与假后端无关，
  // 但只在预览模式下需要，所以挂在这里一起装、一起卸。
  const uninstallProbe = installTopologyProbe();

  // **按事件名分组**记录监听者。
  //
  // 早先是一个扁平的 `Map<id, cb>`，派发时不看事件名 —— 于是推一条日志会把
  // 「延迟结果」的处理器也一起叫醒，它对着日志载荷做 `for...of` 立刻抛
  // `results is not iterable`，React 整棵树卸载（窗口与标签页一起消失）。
  // 真实 Tauri 的事件是按名字派发的，这里必须一样。
  //
  // 关联过程：`listen()` 先调 `transformCallback(cb)` 拿到 id，紧接着
  // `invoke('plugin:event|listen', { event, handler: id })`。所以在
  // `transformCallback` 时记下「刚拿到的 id」，在随后的 listen 调用里
  // 把事件名补上即可（两者在同一个同步段内发生）。
  const listeners = new Map<string, Map<number, (payload: unknown) => void>>();
  let nextId = 1;
  let pendingHandlerId: number | null = null;
  /** 所有登记过的回调（按 id），`listen` 时按事件名归入 `listeners`。 */
  const allCallbacks = new Map<number, (payload: unknown) => void>();

  const internals = {
    invoke: async (cmd: string, _args?: unknown): Promise<unknown> => {
      // 事件订阅相关命令：按事件名登记/注销监听者。
      if (cmd.startsWith("plugin:event|")) {
        const a = (_args ?? {}) as { event?: string; eventId?: number };
        if (cmd === "plugin:event|listen") {
          if (a.event && pendingHandlerId !== null) {
            const bucket = listeners.get(a.event) ?? new Map();
            const cb = allCallbacks.get(pendingHandlerId);
            if (cb) bucket.set(pendingHandlerId, cb);
            listeners.set(a.event, bucket);
            pendingHandlerId = null;
          }
          return nextId++;
        }
        if (cmd === "plugin:event|unlisten") {
          if (a.event && a.eventId !== undefined) {
            listeners.get(a.event)?.delete(a.eventId);
          }
          return null;
        }
        return null;
      }
      switch (cmd) {
        case "snapshot":
          return scenarioSnapshot();
        case "routing_topology":
          return topologyScenario();
        case "globe_data":
          // 用与 Rust 侧一致的形状；坐标取自实测（本机=大理，节点=香港）
          return {
            route: {
              from: { ip: "39.144.146.165", country: "中国", city: "广州市", lat: 23.1317, lon: 113.266, isp: "China Mobile", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 25.61", "ip-api.com: 25.69"] },
              to: { ip: "45.207.197.185", country: "香港", city: "香港", lat: 22.3193, lon: 114.169, isp: "Vapeline Technology", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 22.28", "ip-api.com: 22.32"] },
              bytes: 9_846_000_000,
              node_name: "Xray-45.207.197.185",
            },
            origin: { ip: "39.144.146.165", country: "中国", city: "广州市", lat: 23.1317, lon: 113.266, isp: "China Mobile", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 25.61", "ip-api.com: 25.69"] },
            error: null,
          };
        case "explain_dest": {
          // 判定用真实规则会命中哪条 —— 预览里给一个**与真实配置同形**的结果，
          // 便于核对界面文案；真实判定由 Rust 侧完成（已与真实核心对拍）。
          const dest = String((_args as { dest?: string })?.dest ?? "");
          const isCn = /(baidu|qq|taobao|cn$|\.cn$)/i.test(dest);
          const isGoogle = /google|gmail|youtube/i.test(dest);
          const isAds = /doubleclick|ads?\./i.test(dest);
          const tag = isAds ? "preset-ads" : isGoogle ? "preset-proxy-google" : isCn ? "preset-cn-domain" : "internal-fallback";
          const out = isAds ? "block" : isGoogle || !isCn ? "node-n1d232c6b8c7a5004" : "direct";
          return {
            rule_index: 3,
            rule_tag: tag,
            outbound: out,
            reasons: [isAds ? "命中 geosite:category-ads-all" : isGoogle ? "命中 geosite:google" : isCn ? "命中 geosite:cn" : "网络 tcp 匹配"],
            undecidable: [],
          };
        }
        case "tail_logs":
          return MOCK_LOGS;
        case "diagnostics":
          return "XrayTun 0.8.0（预览数据）\nmacOS 26.6.2\n核心 26.9.9\nhelper: 未安装";
        default:
          // 其余命令一律返回当前快照：按钮有反馈、状态不跳动。
          return scenarioSnapshot();
      }
    },
    transformCallback: (cb: (payload: unknown) => void) => {
      const id = nextId++;
      allCallbacks.set(id, cb);
      pendingHandlerId = id; // 紧接着的 listen 调用会用它认领事件名
      return id;
    },
    unregisterCallback: (id: number) => {
      allCallbacks.delete(id);
      for (const bucket of listeners.values()) bucket.delete(id);
    },
    convertFileSrc: (p: string) => p,
  };

  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
  // `@tauri-apps/api/event` 的 `_unlisten` 不走 `invoke`，而是**直接**调
  // `window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener(event, eventId)`。
  // 真实 Tauri 由 webview 注入这个全局；预览里以前没有它，于是每次
  // register/unlisten 都抛 `Cannot read properties of undefined` —— 那是
  // **预览桥接的缺口，不是产品缺陷**（生产路径用的是 Tauri 自己的 internals）。
  // 这里补上按事件名注销的方法，与 `plugin:event|unlisten` 命令保持同一语义。
  (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (event: string, eventId: number) => {
      listeners.get(event)?.delete(eventId);
      allCallbacks.delete(eventId);
    },
  };
  // 页面里的 Tauri API 版本探测会读这个
  (window as unknown as Record<string, unknown>).__TAURI__ = {};

  // 开发用：把一条核心日志推给已经订阅的 handler。
  //
  // 真实的 Tauri 事件由 Rust 侧 `emit`，浏览器里没有那条通路；而「跟随滚动」
  // 这类行为只有**真的来新日志**才能验证（往 DOM 里插元素不会触发 React 的
  // 状态更新）。所以这里按 `core://log` 的载荷形状直接调用监听者。
  (window as unknown as Record<string, unknown>).__emitCoreLog = (line: string, level = "info") => {
    // 只发给订阅了 `core://log` 的那些 —— 与真实事件派发一致。
    const bucket = listeners.get("core://log");
    if (!bucket) return;
    for (const cb of bucket.values()) {
      cb({ event: "core://log", payload: { line, level } });
    }
  };

  return () => {
    uninstallProbe();
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__emitCoreLog;
    listeners.clear();
    allCallbacks.clear();
  };
}
