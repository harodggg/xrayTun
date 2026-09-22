/**
 * 预览桥接 · 假快照（`snapshot` 命令）。
 *
 * `?preview=1&state=connected|uncommitted|disconnected|no-core|stale|notice`
 * 决定返回哪一种状态 —— 这样每种异常态都能在预览里看到、能截图、能回归。
 * 场景开关的解析就放在数据旁边，**不要再散到别处**。
 *
 * 字段名由 `apps/desktop/tests/type_contract.rs` 保证与 Rust 一致；
 * 取值只需「看起来像真的」。
 */

import type { AppSnapshot, ProbeResult } from "./types";

const now = Math.floor(Date.now() / 1000);

/** 几个节点，覆盖「快 / 慢 / 连不上」三种呈现，方便检查状态色与文案。 */
const NODES: AppSnapshot["nodes"] = [
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
];

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
/**
 * 预览用的「GitHub 上的最新版」条目。
 *
 * `size` / `published_at` / `download_url` 用 v0.8.28 的真实形状（站点资产表里有同样的
 * 数字），这样界面上的文案与真实数据同形。
 */
function fakeAppRelease(version: string): NonNullable<AppSnapshot["update"]["latest_app"]> {
  return {
    size: 47_145_126,
    version,
    published_at: "2026-09-20T16:03:44Z",
    prerelease: false,
    download_url: `https://github.com/harodggg/xrayTun/releases/download/v${version}/XrayTun_${version}_x86_64_arm64.dmg`,
    digest_url: `https://github.com/harodggg/xrayTun/releases/download/v${version}/SHA256SUMS.txt`,
  };
}

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
      base.helper = { ...base.helper, socket_present: true, reachable: true, version: "0.8.0", protocol: 1, tun_active: true, state: "ready" };
      break;
    case "disconnected":
      base.settings.mode = "tun";
      base.runtime = { ...base.runtime, running: false, pid: null, started_at_unix: null, tun_interface: null, routes_committed: false, config_path: null, last_good_node: null };
      base.traffic = { rx_bytes: 8_412_774_400, tx_bytes: 1_204_887_552, rx_rate: 0, tx_rate: 0 };
      base.notice = null;
      break;
    case "no-core":
      base.core = { ...base.core, path: null, version: null, supports_native_tun: false, error: "找不到 Xray 核心可执行文件" };
      base.notice = null;
      break;
    case "stale":
      base.helper = { ...base.helper, stale_session: "sess-7f3a91", state: "not_running" };
      break;
    case "notice":
      // 故意堆多条，检查「只显示最急一条 + 还有 N 条」
      base.core = { ...base.core, supports_native_tun: false };
      base.runtime.last_error = "上次启动失败：端口 10808 被占用";
      break;
    case "update-latest":
      // 「已经是最新版」：**查得到** GitHub 上的版本，但当前装的就是它。
      // 这正是用户报的「多余」场景 —— 应该没有更新按钮、但要有「已是最新版本」。
      base.update = {
        ...base.update,
        latest_app: fakeAppRelease(base.app_version),
        app_update_available: false,
      };
      break;
    case "update-available":
      // 确实有新版：必须出现「更新到 X 并重启」。
      base.update = {
        ...base.update,
        latest_app: fakeAppRelease("0.8.30"),
        app_update_available: true,
      };
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
    // ⚠️ task-87：这里曾经是**另一套字段名** —— `capture_ipv6` / `ipv6_mode` /
    // `bypass_hosts` / `install_default_routes` / `dns_handling` / `fake_dns`
    // **全都不在 `TunSettings` 里**，而真字段 `network` / `sentinel_dns` / `ipv6` /
    // `bypass_private` / `bind_outbound_to` 一个都没有 ⇒ 预览里那几个输入是**空的**，
    // 而整块被 `as unknown as` 挡住，编译器一声不吭。现在逐字段与 `TunSettings` 对齐。
    tun: {
      mtu: 1500,
      network: "198.18.0.1/15",
      sentinel_dns: "198.18.0.2",
      ipv6: "passthrough",
      bypass_private: true,
      datapath: "xray_native_tun",
      fd_ownership: "helper_holds",
      bind_outbound_to: null,
    },
    // 同族的第二处：旧版是 `fake_dns` / `fallback_servers` 这套**不存在的字段**，
    // 而 `auto_select` / `mode` / `disable_cache` / `sniffing` 都没给。
    dns: {
      auto_select: false,
      mode: "split_by_rule",
      remote_servers: ["https://1.1.1.1/dns-query"],
      direct_servers: ["223.5.5.5", "119.29.29.29"],
      hosts: [],
      query_strategy: "use_ip",
      disable_cache: false,
      sniffing: true,
    },
    // `FakeDnsSettings` = { enabled, ip_pool, pool_size }；默认值取 Rust 的
    // `default_fake_ip_pool()` = "198.18.0.0/16" 与 `default_fake_ip_pool_size()` = 65535
    fakedns: { enabled: true, ip_pool: "198.18.0.0/16", pool_size: 65535 },
    core_path: null,
    launch_at_login: true,
    log_level: "info",
    restore_system_proxy_on_exit: true,
    // `auto_reconnect` 的 Rust 默认值是 `true`（`#[serde(default = "yes")]`）——
    // 预览取**与真实一致**的值，不再靠「字段缺失 ⇒ 界面按默认值显示」蒙过去（task-89）。
    auto_reconnect: true,
    show_speed_in_title: false,
  },
  subscriptions: [
    {
      id: "sub-1",
      name: "主订阅 · 机场 A",
      url: "https://sub.example.com/api/v1/client/subscribe?token=redacted",
      enabled: true,
      update_interval_hours: 24,
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
    },
    {
      id: "sub-2",
      name: "备用 · 机场 B",
      url: "https://sub2.example.net/link/redacted",
      enabled: true,
      update_interval_hours: 24,
      node_count: 1,
      last_updated: now - 86_400 * 3,
      last_error: "HTTP 403：订阅 token 可能已过期",
      usage: null,
    },
    {
      // 第三种形状：**不限量（total == 0）但有有效期**。
      // 后端把 total == 0 定义为「不限量」，这种订阅没有用量比例可画，
      // 但到期时间仍然必须显示（曾经被整块藏掉，是个回归）。
      id: "sub-3",
      name: "不限量 · 机场 C",
      url: "https://sub3.example.org/link/redacted",
      enabled: true,
      update_interval_hours: 24,
      node_count: 0,
      last_updated: now - 600,
      last_error: null,
      usage: { upload: 1_073_741_824, download: 5_368_709_120, total: 0, expire: now + 86_400 * 120 },
    },
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
    // `CoreRuntime` 还有 `recovery`（task-87 补）：空闲态
    recovery: {
      recovering: false,
      attempt: 0,
      probe_failures: 0,
      started_unix: null,
      last_outcome: null,
      finished_unix: null,
    },
  },
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
    // ⚠️ task-87：预览**不读磁盘上的助手二进制**（那是真机才做的探测），所以这里给一个
    // **明确的预览态** —— 而不是假装成 `match`/`mismatch` 去伪造一个真实结论。
    // 界面会按 `unreadable` 如实显示「无法核对助手版本：<这个 reason>」。
    version_check: {
      state: "unreadable",
      installed: null,
      bundled: null,
      reason: "预览模式不读取磁盘上的助手版本",
    },
  },
  core: {
    path: "/Applications/XrayTun.app/Contents/Resources/xray",
    version: "26.9.9",
    error: null,
    supports_native_tun: true,
    min_native_tun_version: "26.1.31",
  },
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
    /** 后端比过版本才算出来的字段（见 types.ts 的说明）。 */
    app_update_available: false,
    checked_at: now - 300,
    check_error: null,
    progress: null,
  },
  dns: {
    // `DnsProbe` = { server, label, kind, transport, latency_ms, answered, suspect, note }；
    // 旧版写的是 `kind: "direct"`（真实取值只有 domestic/foreign）与 `error`（应为 note）
    // ⇒ 国内探测项在预览里**根本对不上类型**。
    probes: [
      { server: "223.5.5.5", label: "阿里 DNS", kind: "domestic", transport: "plain_udp", latency_ms: 12, answered: true, suspect: false, note: null },
      { server: "119.29.29.29", label: "腾讯 DNS", kind: "domestic", transport: "plain_udp", latency_ms: 18, answered: true, suspect: false, note: null },
      { server: "https://1.1.1.1/dns-query", label: "Cloudflare DoH", kind: "foreign", transport: "doh", latency_ms: 148, answered: true, suspect: false, note: null },
    ],
    chosen: "223.5.5.5",
    chosen_foreign: "https://1.1.1.1/dns-query",
    probed_at: now - 600,
    error: null,
    foreign_error: null,
  },
  app_version: "0.8.0",
};
