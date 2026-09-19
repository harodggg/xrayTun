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
  traffic_error: null,
  geo_available: true,
};

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

/** 安装桥接。返回 uninstall，便于热更新时清理。 */
export function installPreviewBridge(): () => void {
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
          return MOCK_TOPOLOGY;
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
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__emitCoreLog;
    listeners.clear();
    allCallbacks.clear();
  };
}
