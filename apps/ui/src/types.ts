//! 与 Rust 侧 `serde` 结构一一对应的类型。
//!
//! 这些类型**没有**代码生成，是手写同步的。代价是改 Rust 结构时要记得改这里；
//! 收益是零构建步骤、零额外依赖。为了降低漏改风险：
//!
//! * Rust 侧所有面向 UI 的结构都在 `crates/xt-proto/src/lib.rs` 和
//!   `apps/desktop/src/state.rs` 里集中定义，改动点很少；
//! * 所有可能新增的枚举值都在 UI 侧做了穷尽处理 + 兜底文案，
//!   新增一个值最坏情况是显示原始字符串，而不是崩掉。

export type ProxyMode = "direct" | "system_proxy" | "tun";
export type RoutingPreset =
  | "global_proxy"
  | "bypass_mainland"
  | "whitelist_proxy"
  | "direct_all"
  | "custom";
export type DnsHandling = "proxy" | "split_by_rule" | "direct" | "custom";
export type Ipv6Mode = "passthrough" | "override" | "disabled";
export type DatapathMode = "xray_native_tun" | "external_tun2_socks" | "handoff_fd_only";
export type FdOwnership = "handoff_to_caller" | "helper_holds";
export type RuleAction =
  | { kind: "proxy"; outbound: string | null }
  | { kind: "direct" }
  | { kind: "block" };
export type Network = "both" | "tcp" | "udp";
export type NodeSource = { kind: "subscription"; id: string } | { kind: "manual" };

export type VmessSecurity = "auto" | "none" | "zero" | "aes128_gcm" | "chacha20_poly1305";

export type Protocol =
  | { kind: "vmess"; uuid: string; alter_id: number; security: VmessSecurity }
  | { kind: "vless"; uuid: string; flow: string; encryption: string }
  | { kind: "trojan"; password: string }
  | { kind: "shadowsocks"; method: string; password: string; uot: boolean }
  | { kind: "socks"; username: string; password: string }
  | { kind: "http"; username: string; password: string };

export type Transport =
  | { kind: "tcp" }
  | { kind: "web_socket"; path: string; host: string }
  | { kind: "grpc"; service_name: string; multi_mode: boolean }
  | { kind: "http_upgrade"; path: string; host: string }
  | { kind: "xhttp"; path: string; host: string; mode: string }
  | { kind: "quic"; key: string; security: string }
  | { kind: "kcp"; header_type: string; seed: string }
  | { kind: "http"; host: string; path: string };

export interface RealitySettings {
  public_key: string;
  short_id: string;
  spider_x: string;
}

export interface TlsSettings {
  enabled: boolean;
  server_name: string;
  allow_insecure: boolean;
  alpn: string[];
  fingerprint: string;
  reality: RealitySettings | null;
}

export interface MuxSettings {
  enabled: boolean;
  concurrency: number;
  xudp_concurrency: number;
}

export interface Node {
  id: string;
  name: string;
  address: string;
  port: number;
  protocol: Protocol;
  transport: Transport;
  tls: TlsSettings;
  mux: MuxSettings | null;
  source: NodeSource;
  tags: string[];
  raw_uri: string | null;
}

export interface SubscriptionUsage {
  upload: number;
  download: number;
  total: number;
  expire: number | null;
}

export interface Subscription {
  id: string;
  name: string;
  url: string;
  enabled: boolean;
  update_interval_hours: number;
  last_updated: number | null;
  last_error: string | null;
  node_count: number;
  usage: SubscriptionUsage | null;
}

export interface MatchCondition {
  domains: string[];
  ip: string[];
  ports: unknown[];
  source_ip: string[];
  inbound_tags: string[];
  network: Network;
  process_names: string[];
  protocols: string[];
}

export interface RoutingRule {
  id: string;
  name: string;
  enabled: boolean;
  when: MatchCondition;
  then: RuleAction;
}

export interface DnsSettings {
  /** 启动时自动探测并把最快的解析器排到前面。 */
  auto_select: boolean;
  mode: DnsHandling;
  remote_servers: string[];
  direct_servers: string[];
  hosts: [string, string][];
  query_strategy: string;
  disable_cache: boolean;
  sniffing: boolean;
}

export interface FakeDnsSettings {
  enabled: boolean;
  ip_pool: string;
  pool_size: number;
}

export interface TunSettings {
  mtu: number;
  network: string;
  sentinel_dns: string;
  ipv6: Ipv6Mode;
  bypass_private: boolean;
  datapath: DatapathMode;
  fd_ownership: FdOwnership;
  bind_outbound_to: string | null;
}

export interface AppSettings {
  mode: ProxyMode;
  socks_port: number;
  http_port: number;
  allow_lan: boolean;
  selected_node: string | null;
  routing_preset: RoutingPreset;
  custom_rules: RoutingRule[];
  tun: TunSettings;
  dns: DnsSettings;
  fakedns: FakeDnsSettings;
  core_path: string | null;
  launch_at_login: boolean;
  log_level: string;
  restore_system_proxy_on_exit: boolean;
  /** 实时网速显示在窗口标题栏与菜单栏。 */
  show_speed_in_title: boolean;
}

/** 开机自启动的**真实**状态，来自系统的 SMAppService，不是回显设置字段。 */
export interface LoginItemState {
  status: "not_registered" | "enabled" | "requires_approval" | "not_found" | "error";
  detail: string;
  needs_approval: boolean;
}

export interface CoreRuntime {
  running: boolean;
  pid: number | null;
  started_at_unix: number | null;
  config_path: string | null;
  tun_session: string | null;
  tun_interface: string | null;
  routes_committed: boolean;
  last_error: string | null;
}

export type HelperState =
  | "ready"
  | "not_installed"
  | "not_running"
  | "not_permitted"
  | "needs_approval"
  | "unknown";

export interface HelperAvailability {
  state: HelperState;
  socket_present: boolean;
  reachable: boolean;
  version: string | null;
  protocol: number | null;
  tun_active: boolean;
  stale_session: string | null;
  needs_approval: boolean;
  error: string | null;
}

export interface CoreAvailability {
  path: string | null;
  version: string | null;
  error: string | null;
  supports_native_tun: boolean;
  min_native_tun_version: string;
}

export interface TrafficSample {
  rx_bytes: number;
  tx_bytes: number;
  rx_rate: number;
  tx_rate: number;
}

/** 导出节点的结果：分享链接 + 二维码（内联 SVG）+ 丢失字段说明。 */
export interface NodeExport {
  node_id: string;
  node_name: string;
  uri: string;
  svg: string;
  /** 分享链接表达不了、因此没能带出去的字段。非空时必须显示给用户。 */
  lost: string[];
}

/** 一次可用的更新。 */
export interface AvailableUpdate {
  /** 产物字节数；上游没报时为 null。 */
  size: number | null;
  version: string;
  published_at: string;
  prerelease: boolean;
  download_url: string;
  digest_url: string | null;
}

export interface DnsProbe {
  server: string;
  label: string;
  kind: "domestic" | "foreign";
  transport: "plain_udp" | "doh";
  latency_ms: number | null;
  answered: boolean;
  suspect: boolean;
  /** 有值时表示「没测」（例如节点未连接），不能当成「不通」。 */
  note: string | null;
}

export interface DnsStatus {
  probes: DnsProbe[];
  /** 国内组首选（direct_servers[0]）。 */
  chosen: string | null;
  /** 国外组首选（remote_servers[0]）。 */
  chosen_foreign: string | null;
  probed_at: number | null;
  error: string | null;
  foreign_error: string | null;
}

/** 一次更新下载的进度，用于进度条。 */
export interface UpdateProgress {
  label: string;
  done_bytes: number;
  /** 上游没报字节数时为 null —— 界面退化成「只显示已下载多少」。 */
  total_bytes: number | null;
}

export interface UpdateStatus {
  core_version: string | null;
  core_managed: boolean;
  core_managed_version: string | null;
  geo_tag: string | null;
  geo_installed_at: number | null;
  latest_core: AvailableUpdate | null;
  latest_geo: AvailableUpdate | null;
  /** 客户端自己的最新版。仓库是私有的，所以这一步需要 token。 */
  latest_app: AvailableUpdate | null;
  /** 正在进行的更新下载。null 表示没有在下载。 */
  progress: UpdateProgress | null;
  checked_at: number | null;
  check_error: string | null;
}

export interface ProbeResult {
  node_id: string;
  node_name: string;
  /** **延迟：本地 → 服务器的 TCP 握手 RTT**（3 次中位数）。主指标。 */
  server_rtt_ms: number | null;
  /** 经这个节点能不能真的取到东西。只取成功/失败。 */
  available: boolean;
  /** 经节点到靶点的 TTFB。仅供诊断 —— 含「服务器→靶点」那段，不是延迟。 */
  through_node_ms: number | null;
  http_status: number | null;
  error: string | null;
  tested_at: number;
}

export interface LogEntry {
  ts_unix: number;
  source: string;
  level: string;
  message: string;
}

export interface AppSnapshot {
  settings: AppSettings;
  subscriptions: Subscription[];
  nodes: Node[];
  runtime: CoreRuntime;
  latency: Record<string, ProbeResult>;
  traffic: TrafficSample;
  notice: string | null;
  helper: HelperAvailability;
  core: CoreAvailability;
  login_item: LoginItemState;
  update: UpdateStatus;
  dns: DnsStatus;
  app_version: string;
}

// ---------------------------------------------------------------------------
// 展示辅助（纯函数，放在类型旁边方便复用）
// ---------------------------------------------------------------------------

export const MODE_LABEL: Record<ProxyMode, string> = {
  direct: "直连",
  system_proxy: "系统代理",
  tun: "TUN 模式",
};

export const PRESET_LABEL: Record<RoutingPreset, string> = {
  global_proxy: "全局代理",
  bypass_mainland: "绕过大陆",
  whitelist_proxy: "白名单代理",
  direct_all: "全部直连",
  custom: "自定义",
};

export const DNS_MODE_LABEL: Record<DnsHandling, string> = {
  proxy: "全部走代理解析",
  split_by_rule: "按规则分流解析",
  direct: "全部本地解析",
  custom: "自定义服务器",
};

export const IPV6_LABEL: Record<Ipv6Mode, string> = {
  passthrough: "不接管（走物理网卡）",
  override: "同样接管 IPv6",
  disabled: "禁用 IPv6",
};

/** 节点摘要，等价于 Rust 侧的 `Node::summary()`。 */
export function nodeSummary(node: Node): string {
  const parts = [protocolName(node.protocol), transportName(node.transport)];
  const security = tlsSecurity(node.tls);
  if (security !== "none") parts.push(security);
  return parts.join(" + ");
}

export function protocolName(p: Protocol): string {
  switch (p.kind) {
    case "vmess":
      return "vmess";
    case "vless":
      return "vless";
    case "trojan":
      return "trojan";
    case "shadowsocks":
      return "shadowsocks";
    case "socks":
      return "socks";
    case "http":
      return "http";
    default:
      // 新增了协议但 UI 没跟上时，显示原始字符串而不是崩掉。
      return (p as { kind: string }).kind;
  }
}

export function transportName(t: Transport): string {
  switch (t.kind) {
    case "tcp":
      return "tcp";
    case "web_socket":
      return "ws";
    case "grpc":
      return "grpc";
    case "http_upgrade":
      return "httpupgrade";
    case "xhttp":
      return "xhttp";
    case "quic":
      return "quic";
    case "kcp":
      return "kcp";
    case "http":
      return "h2";
    default:
      return (t as { kind: string }).kind;
  }
}

export function tlsSecurity(t: TlsSettings): "tls" | "reality" | "none" {
  if (t.reality) return "reality";
  if (t.enabled) return "tls";
  return "none";
}

export function ruleActionLabel(action: RuleAction): string {
  switch (action.kind) {
    case "proxy":
      return action.outbound ? `代理(${action.outbound})` : "代理";
    case "direct":
      return "直连";
    case "block":
      return "拦截";
    default:
      return "未知";
  }
}

export function formatBytes(n: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB"];
  let value = n;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value.toFixed(unit === 0 ? 0 : 2)} ${units[unit]}`;
}

export function formatRate(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`;
}

export function formatTimestamp(unixSeconds: number | null): string {
  if (!unixSeconds) return "从未";
  return new Date(unixSeconds * 1000).toLocaleString("zh-CN", { hour12: false });
}

/** 延迟分档，用于列表着色。 */
export function latencyTier(ms: number | null | undefined): "unknown" | "fast" | "ok" | "slow" {
  if (ms === null || ms === undefined) return "unknown";
  if (ms < 150) return "fast";
  if (ms < 400) return "ok";
  return "slow";
}

// ---------------------------------------------------------------------------
// 网络流动拓扑
// ---------------------------------------------------------------------------

export interface TopoInbound {
  tag: string;
  protocol: string;
  port: number | null;
  /**
   * 累计字节（跨核心重启保持单调）。
   * `Topology.traffic_ok === false` 时这两个字段固定为 0，**不是**真实读数。
   */
  uplink_bytes: number;
  downlink_bytes: number;
}

export interface TopoOutbound {
  tag: string;
  protocol: string;
  /** node | direct | block | dns | internal */
  kind: string;
  /**
   * 累计字节（跨核心重启保持单调）。
   * `Topology.traffic_ok === false` 时这两个字段固定为 0，**不是**真实读数。
   */
  uplink_bytes: number;
  downlink_bytes: number;
}

export interface TopoRule {
  index: number;
  tag: string;
  outbound: string;
  /** 人类可读的条件摘要，例如 `域名 geosite:cn` */
  conditions: string[];
}

export interface Topology {
  inbound: TopoInbound[];
  rule: TopoRule[];
  outbound: TopoOutbound[];
  /** 取流量失败的原因（核心没在跑时）。界面据此如实说明，而不是画 0 流量。 */
  traffic_error: string | null;
  /**
   * 本次流量是否可信。`false` = 这次没查到（原因见 `traffic_error`），
   * 此时所有 `*_bytes` 都是占位 0，界面必须显示「—」而不是 0 B。
   *
   * 恒有 `traffic_ok === (traffic_error === null)`，两者不会不一致。
   */
  traffic_ok: boolean;
  /**
   * 累计字节跨核心重启续接时，被补偿掉的归零次数。
   * `> 0` 表示核心重启过（换网 / 熄屏唤醒 / 节点抖动），
   * 累计值已被续接而不是归零；界面可据此如实说明，而不用平滑掩盖。
   */
  counter_resets: number;
  geo_available: boolean;
}

export interface RouteExplanation {
  rule_index: number | null;
  rule_tag: string | null;
  outbound: string;
  reasons: string[];
  undecidable: string[];
}

/** IP 的地理位置（来自 ip-api.com）。 */
export interface GeoLocation {
  ip: string;
  country: string;
  city: string;
  lat: number;
  lon: number;
  isp: string;
  /** 坐标来源。 */
  source: string;
  /** 多个数据源对同一 IP 的判定是否一致。 */
  consistent: boolean;
  /** 各数据源的判定摘要（便于展示分歧）。 */
  sources: string[];
}

/** 地球仪上的一条航线：本机 → 出口节点。 */
export interface GlobeRoute {
  from: GeoLocation;
  to: GeoLocation;
  /** 这条航线当前承载的实测字节（上行+下行，跨核心重启保持单调）。 */
  bytes: number;
  /** 取流量失败时为 false，此时 `bytes` 固定为 0，不是真实读数。 */
  traffic_ok: boolean;
  /** 累计值跨核心重启续接时被补偿掉的归零次数。 */
  counter_resets: number;
  node_name: string;
}

export interface GlobeData {
  route: GlobeRoute | null;
  origin: GeoLocation | null;
  /** 拿不到位置时的原因。 */
  error: string | null;
}
