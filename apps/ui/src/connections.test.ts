/**
 * 单连接可视化的**匹配与过滤**逻辑测试。
 *
 * # 为什么这些必须是纯函数单测
 *
 * 「日志连接 × 拓扑」的全部风险集中在两处：
 *
 * 1. **匹配**：连接的 `[入站 → 出站]` 能不能正确落到拓扑卡片上。
 *    匹配不到时**必须给解释**（内部通道 / 配置刚变过）—— 静默不高亮
 *    和「这条连接本来就不在流向图里」是两件事，用户必须能分清。
 * 2. **过滤**：连接很密（实测 14 秒 100+ 条），过滤写错会让人以为
 *    「这个域名没有连接」。
 *
 * 这两件事都不依赖渲染，用纯函数测最可靠；**渲染层的高亮**由 CDP 在真实
 * 浏览器里核对（见 docs/ui/topology/CONNECTIONS.md 的验收方式）——
 * jsdom 没有布局引擎，`getBoundingClientRect` 全 0，路径几何在 jsdom 里
 * 根本量不出来，硬测只会测出假绿。
 */

import { describe, expect, it } from "vitest";

import {
  CONNECTION_ROW_LIMIT,
  connectionKey,
  filterConnections,
  matchConnectionToTopology,
} from "./pages/Topology";
import type { ConnectionFilter, TopologyTags } from "./pages/Topology";
import type { ConnectionRecord } from "./types";

const TAGS: TopologyTags = {
  flowInlets: ["tun", "socks", "http"],
  internalInlets: ["api"],
  flowOutlets: ["node-n1d232c6b8c7a5004", "direct", "block"],
  internalOutlets: ["dns-out", "api"],
};

function conn(over: Partial<ConnectionRecord> = {}): ConnectionRecord {
  return {
    ts_ms: 1_759_000_000_000,
    ts_text: "2026/09/20 13:30:58.560364",
    from: "198.18.0.1:49712",
    network: "tcp",
    target_host: "194.221.250.50",
    target_port: 443,
    inbound_tag: "tun",
    outbound_tag: "node-n1d232c6b8c7a5004",
    domain: "www.google.com",
    domain_paired: true,
    domain_pair_delta_us: 26,
    sniff_id: "3163266252",
    ...over,
  };
}

const NO_FILTER: ConnectionFilter = { inbound: "", outbound: "", query: "" };

describe("连接 → 拓扑的匹配", () => {
  it("普通连接：入站与出站都命中，且能在流向图上画线", () => {
    // 防的故障：高亮找错卡片 / 本该画线却不画。
    const m = matchConnectionToTopology(conn(), TAGS);
    expect(m.inlet).toBe("tun");
    expect(m.outlet).toBe("node-n1d232c6b8c7a5004");
    expect(m.inFlow).toBe(true);
    expect(m.note).toBeNull();
  });

  it("内部入站（api）：不在流向图里，必须给出解释而不是静默", () => {
    // 防的故障：日志里大量 api 回环连接（实测 5374 条）；静默不高亮
    // 会让用户以为「界面坏了」。
    const m = matchConnectionToTopology(conn({ inbound_tag: "api", outbound_tag: "api" }), TAGS);
    expect(m.inlet).toBeNull();
    expect(m.internalInbound).toBe(true);
    expect(m.inFlow).toBe(false);
    expect(m.note).toContain("内部通道");
  });

  it("内部出站（dns-out）：匹配到卡片但不画线", () => {
    // 防的故障：dns-out 的字节计数是测量盲区，它在出口列单独一组；
    // 高亮必须能找到它，但不能在流向图上凭空画一条线。
    const m = matchConnectionToTopology(conn({ outbound_tag: "dns-out" }), TAGS);
    expect(m.outlet).toBe("dns-out");
    expect(m.internalOutbound).toBe(true);
    expect(m.inFlow).toBe(false);
    expect(m.note).toContain("内部通道");
  });

  it("两边都不在拓扑里（配置刚换过）：明确说「不在当前拓扑里」", () => {
    const m = matchConnectionToTopology(
      conn({ inbound_tag: "old-in", outbound_tag: "old-out" }),
      TAGS,
    );
    expect(m.inlet).toBeNull();
    expect(m.outlet).toBeNull();
    expect(m.note).toContain("不在当前拓扑里");
  });

  it("入口命中、出站是内部通道：入口高亮，并说明出站为何没有线", () => {
    const m = matchConnectionToTopology(conn({ outbound_tag: "api" }), TAGS);
    expect(m.inlet).toBe("tun");
    expect(m.outlet).toBe("api");
    expect(m.internalOutbound).toBe(true);
    expect(m.inFlow).toBe(false);
    expect(m.note).toContain("内部通道");
  });
});

describe("连接过滤", () => {
  const items = [
    conn({ ts_ms: 3, domain: "www.google.com", target_host: "142.250.72.14", outbound_tag: "node-n1d232c6b8c7a5004" }),
    conn({ ts_ms: 2, domain: null, target_host: "110.242.68.66", outbound_tag: "direct", inbound_tag: "socks" }),
    conn({ ts_ms: 1, domain: "doubleclick.net", target_host: "20.205.243.166", outbound_tag: "block" }),
  ];

  it("空条件不做拷贝（密集列表下这是每帧都会走的热路径）", () => {
    expect(filterConnections(items, NO_FILTER)).toBe(items);
  });

  it("按入站过滤", () => {
    const r = filterConnections(items, { ...NO_FILTER, inbound: "socks" });
    expect(r).toHaveLength(1);
    expect(r[0]!.inbound_tag).toBe("socks");
  });

  it("按出站过滤", () => {
    expect(filterConnections(items, { ...NO_FILTER, outbound: "block" })).toHaveLength(1);
  });

  it("按域名子串过滤，且大小写不敏感", () => {
    expect(filterConnections(items, { ...NO_FILTER, query: "GOOGLE" })).toHaveLength(1);
  });

  it("域名查询也匹配目标 host（用户可能复制的是 IP）", () => {
    expect(filterConnections(items, { ...NO_FILTER, query: "110.242" })).toHaveLength(1);
  });

  it("domain 为 null 的记录不会被域名查询误命中（但不能崩）", () => {
    expect(filterConnections(items, { ...NO_FILTER, query: "zzz" })).toHaveLength(0);
  });

  it("多个条件是与关系", () => {
    expect(
      filterConnections(items, { inbound: "tun", outbound: "block", query: "doubleclick" }),
    ).toHaveLength(1);
    expect(
      filterConnections(items, { inbound: "socks", outbound: "block", query: "" }),
    ).toHaveLength(0);
  });
});

describe("列表 key 与渲染上界", () => {
  it("同一条连接的 key 稳定", () => {
    expect(connectionKey(conn())).toBe(connectionKey(conn()));
  });

  it("时间/来源/目标/入出站任一不同 → key 不同", () => {
    // 防的故障：key 撞了 → React 复用错行 → 详情与高亮显示的是**另一条**连接。
    const base = conn();
    expect(connectionKey(conn({ ts_ms: base.ts_ms + 1 }))).not.toBe(connectionKey(base));
    expect(connectionKey(conn({ from: "198.18.0.1:1" }))).not.toBe(connectionKey(base));
    expect(connectionKey(conn({ target_host: "1.1.1.1" }))).not.toBe(connectionKey(base));
    expect(connectionKey(conn({ outbound_tag: "direct" }))).not.toBe(connectionKey(base));
  });

  it("列表渲染上界是有界的（密集连接下不能全渲染）", () => {
    expect(CONNECTION_ROW_LIMIT).toBeGreaterThan(0);
    expect(CONNECTION_ROW_LIMIT).toBeLessThanOrEqual(200);
  });
});
