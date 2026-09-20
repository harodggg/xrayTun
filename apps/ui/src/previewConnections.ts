/**
 * 预览桥接 · 假连接（`recent_connections` 命令，单连接可视化）。
 *
 * 场景开关 `?preview=1&connections=normal|busy|empty|unavailable`：
 * busy 造 600 条（验「只渲染最近 N 条」与过滤性能）、empty 真空态、
 * unavailable 让命令 reject（验异常态文案）。
 *
 * 时间锚在模块加载时，**每次轮询返回同一份** —— 截图与回归要可复现；
 * 「在动」不是这一页要验证的东西。形状以 `types.ts` 的
 * `ConnectionRecord` / `RecentConnections` 为准（`type_contract.rs` 盯着）。
 */

import type { ConnectionRecord, RecentConnections } from "./types";

const now = Math.floor(Date.now() / 1000);

// ---------------------------------------------------------------------------
// 最近连接（单连接可视化，见 docs/ui/topology/CONNECTIONS.md）
// ---------------------------------------------------------------------------
//
// 为什么要在预览里造这个：连接列表 / 过滤 / 高亮这三样**只有真的有数据**才
// 看得出对错，而真实连接来自核心访问日志（核心没在跑时一条都没有）。
// 场景开关：
//   `?connections=normal`（默认，42 条）
//   `?connections=busy`   （600 条，密集 —— 用来验证「只渲染最近 N 条」与过滤性能）
//   `?connections=empty`  （真的 0 条：空态文案）
//   `?connections=unavailable`（命令 reject：异常态文案）
//
// 时间锚在模块加载时（和 MOCK_LOGS 一样），**每次轮询返回同一份** ——
// 这样截图与回归是确定性的；「在动」不是这一页要验证的东西。
//
// ⚠️ 形状以 `types.ts` 的 `ConnectionRecord` / `RecentConnections` 为准
// （backend-dev 在 task-10 定稿）。改字段名要同时改这里。

const CONN_DOMAINS = [
  "www.google.com",
  "www.youtube.com",
  "mail.google.com",
  "www.baidu.com",
  "www.taobao.com",
  "github.com",
  "api.github.com",
  "registry.npmjs.org",
  "doubleclick.net",
  "cdn.jsdelivr.net",
];

/** 真实日志里目标**多数是 IP，但也会直接给域名**（backend-dev 实测过）。 */
const CONN_HOSTS = [
  "194.221.250.50",
  "142.250.72.14",
  "20.205.243.166",
  "github.com",
  "cp.cloudflare.com",
  "110.242.68.66",
];

/** 按索引确定性地取一个值（不用 Math.random：截图/回归要可复现）。 */
function pick<T>(arr: T[], i: number): T {
  return arr[i % arr.length]!;
}

/** 日志原样墙钟（带微秒，和真实行同形）：`2026/09/20 13:30:58.560364`。 */
function logClock(ms: number, i: number): string {
  const d = new Date(ms);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  const micros = p(d.getMilliseconds(), 3) + p((i * 137) % 1000, 3);
  return `${d.getFullYear()}/${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${micros}`;
}

/** 造一批连接：新的在前，含「配到域名」「没配到」「内部通道」三类。 */
function mockConnections(count: number): ConnectionRecord[] {
  const inbounds = ["tun", "tun", "tun", "socks", "http"];
  const userOut = ["node-n1d232c6b8c7a5004", "node-n1d232c6b8c7a5004", "direct", "block"];
  const rows: ConnectionRecord[] = [];
  // 时间从旧到新递增，每条 ~180ms
  const start = now * 1000 - count * 180;
  for (let i = 0; i < count; i++) {
    const inbound = pick(inbounds, i * 7 + 1);
    // 每 13 条插一条内部通道，用来验证「不在流向图里」的兜底文案
    const internalKind = i % 13 === 5 ? "dns-out" : i % 13 === 11 ? "api" : null;
    const outbound = internalKind ?? pick(userOut, i * 5 + 2);
    const targetPort =
      internalKind === "dns-out" ? 53 : internalKind === "api" ? 10085 : (i % 3 === 0 ? null : 443);
    // 每 4 条里有 1 条没配到域名（真实情况里约一半配不到，含内部通道）
    const paired = i % 4 !== 0 && internalKind === null;
    const domain = paired ? pick(CONN_DOMAINS, i * 3 + 7) : null;
    const clientPort = 40000 + ((i * 37) % 20000);
    const network = internalKind === "dns-out" ? "udp" : i % 17 === 3 ? "https" : "tcp";
    rows.push({
      ts_ms: start + i * 180,
      ts_text: logClock(start + i * 180, i),
      from:
        network === "https"
          ? "DNS"
          : `${inbound === "tun" ? "198.18.0.1" : "127.0.0.1"}:${clientPort}`,
      network,
      target_host: pick(CONN_HOSTS, i * 5 + 1),
      target_port: targetPort,
      inbound_tag: internalKind === "api" ? "api" : inbound,
      outbound_tag: outbound,
      domain: domain,
      domain_paired: paired,
      // 真实实测 p50 = 26µs —— 用毫秒会四舍五入成 0，所以这里是微秒
      domain_pair_delta_us: paired ? 3 + ((i * 11) % 120) : null,
      sniff_id: paired ? String(3163266252 + i) : null,
    });
  }
  // 新的在前
  return rows.reverse();
}

export function connectionsScenario(): RecentConnections {
  const mode = new URLSearchParams(location.search).get("connections") ?? "normal";
  if (mode === "unavailable") {
    // 与真实命令一致：拿不到就 reject，界面必须显示原因而不是空列表。
    throw new Error("核心未运行：访问日志由核心写入，没有新行可读");
  }
  const count = mode === "busy" ? 600 : mode === "empty" ? 0 : 42;
  const items = mockConnections(count);
  const paired = items.filter((c) => c.domain_paired).length;
  // 环形缓冲 1000 条：busy 场景假装已经挤掉过一批，界面要如实说出来。
  const dropped = mode === "busy" ? 137 : 0;
  const accepted = count + dropped;
  return {
    items,
    dropped,
    pairing: {
      accepted,
      paired,
      unpaired: accepted - paired,
      sniffed: paired + (mode === "busy" ? 40 : 3),
      rejected_stale: mode === "busy" ? 12 : 2,
      sniffed_superseded: mode === "busy" ? 5 : 1,
    },
  };
}
