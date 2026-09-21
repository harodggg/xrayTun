/**
 * 拓扑页的**单连接**辅助逻辑：连接 key、与拓扑的匹配、过滤、时间与配对率格式化。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14，纯搬迁、行为不变）。
 * `pages/Topology.tsx` 仍**转发**这些具名导出 —— `connections.test.ts` 是直接从
 * 那里 import 的（它不在本次重构的写入范围内）。
 */

import type { ConnectionRecord, PairingStats } from "../types";

// ---------------------------------------------------------------------------
// 单连接可视化（设计说明：docs/ui/topology/CONNECTIONS.md）
// ---------------------------------------------------------------------------
//
// # 数据现实（写进代码，也写进界面文案）
//
// 一条连接 = 访问日志里的一行 `accepted`。**没有字节数、没有持续时间、没有
// 连接 ID** —— Xray 的 `StatsService` 只有聚合计数器，日志只记建立。所以这里
// 用到的字段就是「能拿到的全部」，界面上不会出现任何推算出来的流量数字。
//
// 域名来自**另一行** `sniffed domain: …`，实测 p50 相差 26µs（p95 129µs），
// 按时序配对得到 —— 是**近似**，所以带 `domain_paired` 标记，界面必须标注。
// 而且**只有约一半连接配得到域名**（IP 直连与内部通道本来就没有 sniffed 行），
// 所以 `domain === null` 是**正常态**，不是错误。

/** 列表行的稳定 key：日志里没有连接 ID，只能用字段拼。 */
export function connectionKey(c: ConnectionRecord): string {
  return `${c.ts_ms}|${c.from}|${c.target_host}:${c.target_port ?? "?"}|${c.inbound_tag}|${c.outbound_tag}`;
}

/** 拓扑里可用的 tag 集合（用于把连接的 `[入站 → 出站]` 映射到卡片）。 */
export interface TopologyTags {
  /** 流向图里的入口（不含 `api` 这类内部入站）。 */
  flowInlets: string[];
  /** 内部入站 tag（`api`）。 */
  internalInlets: string[];
  /** 流向图里的出口（不含 `dns`/`internal`）。 */
  flowOutlets: string[];
  /** 内部通道出口（`dns-out` / `api`）。 */
  internalOutlets: string[];
}

/** 一条连接与拓扑的匹配结果。 */
export interface ConnectionMatch {
  /** 命中的入口卡片 tag；null = 没有对应卡片。 */
  inlet: string | null;
  /** 命中的出口卡片 tag；null = 没有对应卡片。 */
  outlet: string | null;
  /** **能不能在流向图上画线**：入口与出口都在流向图里才行。 */
  inFlow: boolean;
  /** 入站是内部通道（不在流向图里）。 */
  internalInbound: boolean;
  /** 出站是内部通道（不在流向图里）。 */
  internalOutbound: boolean;
  /** 给用户看的解释；一切正常时为 null。 */
  note: string | null;
}

/**
 * 把连接的 `[入站 → 出站]` 映射到拓扑卡片。
 *
 * 这是「日志连接 × 拓扑」的**唯一耦合点**：两侧用的是同一套 tag。
 * 匹配不到时**必须给出解释**（内部通道 / 配置刚换过），不能静默什么都不高亮 ——
 * 「什么都没发生」和「这条连接走的是内部通道」是两件事。
 */
export function matchConnectionToTopology(
  c: ConnectionRecord,
  tags: TopologyTags,
): ConnectionMatch {
  const inlet = tags.flowInlets.includes(c.inbound_tag) ? c.inbound_tag : null;
  const outletInFlow = tags.flowOutlets.includes(c.outbound_tag) ? c.outbound_tag : null;
  const outletInternal = tags.internalOutlets.includes(c.outbound_tag) ? c.outbound_tag : null;
  const internalInbound = inlet === null && tags.internalInlets.includes(c.inbound_tag);
  const internalOutbound = outletInternal !== null;
  const outlet = outletInFlow ?? outletInternal;
  const inFlow = inlet !== null && outletInFlow !== null;

  let note: string | null = null;
  if (inlet === null && !internalInbound) {
    note = `入口「${c.inbound_tag}」不在当前拓扑里（配置可能刚变过）`;
  } else if (internalInbound && internalOutbound) {
    note = `这条连接走的是内部通道（${c.inbound_tag} → ${c.outbound_tag}），不在流向图里`;
  } else if (internalInbound) {
    note = `入站「${c.inbound_tag}」是内部通道，不在流向图里；出站「${c.outbound_tag}」已高亮`;
  } else if (internalOutbound) {
    note = `出站「${c.outbound_tag}」是内部通道（${c.outbound_tag === "dns-out" ? "DNS 劫持" : "本机回环"}），不在流向图里`;
  } else if (outlet === null) {
    note = `出站「${c.outbound_tag}」不在当前拓扑里（配置可能刚变过）`;
  }
  return { inlet, outlet, inFlow, internalInbound, internalOutbound, note };
}

/** 连接列表的过滤条件。空字符串 = 不过滤。 */
export interface ConnectionFilter {
  inbound: string;
  outbound: string;
  /** 域名或目标（`ip:port`）的子串，大小写不敏感。 */
  query: string;
}

/** 过滤最近连接。纯函数，便于单测（密集连接下这是最容易被写错的一处）。 */
export function filterConnections(
  items: ConnectionRecord[],
  f: ConnectionFilter,
): ConnectionRecord[] {
  const q = f.query.trim().toLowerCase();
  if (!f.inbound && !f.outbound && !q) return items;
  return items.filter((c) => {
    if (f.inbound && c.inbound_tag !== f.inbound) return false;
    if (f.outbound && c.outbound_tag !== f.outbound) return false;
    if (q) {
      const target = `${c.target_host}:${c.target_port ?? ""}`;
      const hay = `${c.domain ?? ""} ${target}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

/** 列表最多渲染多少行（连接可达每秒数十条，全渲染会卡）。 */
export const CONNECTION_ROW_LIMIT = 100;

/** `HH:MM:SS.mmm` —— 日志是毫秒级，秒级不够用（同一秒很多条）。 */
function formatConnTime(tsMs: number): string {
  const d = new Date(tsMs);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

/**
 * 列表里显示的时间：优先用**日志原样墙钟**（`ts_text`，微秒精度里取毫秒）——
 * 那是用户能在日志文件里对上的那一串；`ts_ms` 只是本进程收到的时刻。
 */
export function connClock(c: ConnectionRecord): string {
  const m = /(\d{2}:\d{2}:\d{2})\.(\d{3})/.exec(c.ts_text);
  return m ? `${m[1]}.${m[2]}` : formatConnTime(c.ts_ms);
}

/** 配对率（%）。`accepted === 0` 时给 0，不产生 NaN。 */
export function pairingPercent(p: PairingStats): number {
  return p.accepted > 0 ? Math.round((p.paired / p.accepted) * 100) : 0;
}

/** 出口 tag 太长时截断显示（节点 tag 形如 `node-n1d232c6b8c7a5004`）。 */
export function shortTag(t: string): string {
  if (t.startsWith("node-")) return `节点 ${t.slice(5, 13)}…`;
  return t;
}
