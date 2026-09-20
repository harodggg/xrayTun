/**
 * 预览桥接 · 假拓扑（`routing_topology` 命令）。
 *
 * `?preview=1&traffic=unavailable` 模拟「这次没查到流量」：所有字节字段填**占位 0**、
 * `traffic_ok=false`，与后端契约一致。没有这个开关，「查不到流量」这条路径在预览里
 * 永远看不到，前端按 `traffic_ok` 渲染的分支就没法截图、没法回归。
 */

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
    { tag: "node-n1d232c6b8c7a5004", protocol: "vless", kind: "node", uplink_bytes: 1_246_000_000, downlink_bytes: 8_600_000_000, connections: 6227 },
    { tag: "direct", protocol: "freedom", kind: "direct", uplink_bytes: 900_000, downlink_bytes: 120_000_000, connections: 1222 },
    { tag: "block", protocol: "blackhole", kind: "block", uplink_bytes: 12_000, downlink_bytes: 0, connections: 201 },
    // 内部通道：字节计数器恒为 0（StatsService 不统计 UDP 出站），只能看连接数
    { tag: "dns-out", protocol: "dns", kind: "dns", uplink_bytes: 0, downlink_bytes: 0, connections: 4769 },
    // 内部通道：字节计数器恒为 0（StatsService 不统计本机回环）
    { tag: "api", protocol: "freedom", kind: "internal", uplink_bytes: 0, downlink_bytes: 0, connections: 5374 },
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
export function topologyScenario(): typeof MOCK_TOPOLOGY {
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
