/**
 * task-181：A21（本机身份）与 A20（流量归属）的界面如实呈现。
 *
 * 字段来自 `task-179`（后端冻结，字段名与 Rust 的 `SelfCheck` / `TrafficProvenance` 逐字一致）：
 *
 * | 字段 | 呈现口径 |
 * |---|---|
 * | `self_check.trusted === false` | **不许**写「本机 · 」；写「未验证的出口」+ `reason` |
 * | `self_check.ip === null` | 「本机位置未知」 |
 * | `self_check.trusted === true` | 可以说「本机出口」，但要写清是**按接口直查**、多出口时可能不同 |
 * | `traffic.verified === false` | **不显示数字**（`bytes` 是占位）+ 展示 `reason` |
 * | `traffic.verified && is_node_outbound` | 才可以说「节点出站累计」 |
 * | `traffic.verified && !is_node_outbound` | 如实说是**该 tag** 的流量，**不许**算到节点头上 |
 */
import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ globeData: vi.fn() }));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: { globeData: mocks.globeData },
    subscribe: () => () => {},
  };
});

import Globe, {
  originCaveat,
  originLabel,
  trafficLabel,
} from "./pages/Globe";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { GlobeData } from "./types";

const LOC = (ip: string) => ({
  ip,
  country: "中国",
  city: "广州市",
  lat: 23.13,
  lon: 113.27,
  isp: "China Mobile",
  source: "ipwho.is",
  consistent: true,
  sources: ["ipwho.is"],
});

function globe(over: {
  trusted: boolean;
  selfIp?: string | null;
  selfReason?: string | null;
  verified: boolean;
  tag?: string | null;
  isNode?: boolean;
  trafficReason?: string | null;
}): GlobeData {
  return {
    route: {
      from: LOC("39.144.146.165"),
      to: LOC("45.207.197.185"),
      bytes: 9_846_000_000,
      node_name: "Xray-45.207.197.185",
      traffic_ok: over.verified,
      counter_resets: 0,
      traffic: {
        tag: over.verified ? (over.tag ?? "node-n1") : null,
        is_node_outbound: over.verified ? (over.isNode ?? true) : false,
        verified: over.verified,
        reason: over.verified ? null : (over.trafficReason ?? "查统计失败（核心没在跑）⇒ 归属未验证"),
      },
    },
    origin: LOC("39.144.146.165"),
    error: null,
    self_check: {
      ip: over.selfIp === undefined ? "39.144.146.165" : over.selfIp,
      bound_interface: over.trusted ? "en0" : null,
      trusted: over.trusted,
      reason: over.trusted ? null : (over.selfReason ?? "读不到物理默认路由 ⇒ 查询走系统默认路由"),
    },
  };
}

async function renderGlobe(data: GlobeData) {
  mocks.globeData.mockResolvedValue(data);
  render(
    <StoreProvider>
      <Globe />
    </StoreProvider>,
  );
  await waitFor(() => expect(mocks.globeData).toHaveBeenCalled());
  // 等事实卡片渲染出来（两端的 label 之一）
  await screen.findByText(/本机出口|未验证的出口|本机位置未知/);
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.globeData.mockResolvedValue(globe({ trusted: true, verified: true }));
  // 快照（StoreProvider 需要）；Globe 自己不读快照字段
  void scenarioSnapshot;
});

// ---------------------------------------------------------------------------
// 纯函数层
// ---------------------------------------------------------------------------

describe("task-181 · 两套文案的判据", () => {
  it("`originLabel`：可信才是「本机出口」；不可信是「未验证的出口」；ip 为 null 是「本机位置未知」", () => {
    expect(originLabel({ ip: "1.2.3.4", bound_interface: "en0", trusted: true, reason: null })).toBe(
      "本机出口",
    );
    expect(originLabel({ ip: "1.2.3.4", bound_interface: null, trusted: false, reason: "r" })).toBe(
      "未验证的出口",
    );
    expect(originLabel({ ip: null, bound_interface: null, trusted: false, reason: "r" })).toBe(
      "本机位置未知",
    );
  });

  it("`originCaveat`：可信时写清「按接口直查、多出口时可能不同」；不可信时带上后端的原因", () => {
    expect(
      originCaveat({ ip: "1.2.3.4", bound_interface: "en0", trusted: true, reason: null }),
    ).toContain("按物理网卡 en0 直查");
    const bad = originCaveat({
      ip: "1.2.3.4",
      bound_interface: null,
      trusted: false,
      reason: "隧道开着时那就是节点出口",
    });
    expect(bad).toContain("未验证");
    expect(bad, "必须展示后端给的原因").toContain("节点出口");
  });

  it("`trafficLabel`：三种归属三种说法", () => {
    expect(
      trafficLabel({ tag: "node-n1", is_node_outbound: true, verified: true, reason: null }),
    ).toBe("节点出站累计（出站 node-n1）");
    expect(
      trafficLabel({ tag: "direct", is_node_outbound: false, verified: true, reason: null }),
    ).toContain("direct");
    expect(
      trafficLabel({ tag: "direct", is_node_outbound: false, verified: true, reason: null }),
    ).toContain("不是节点出站");
    expect(
      trafficLabel({ tag: null, is_node_outbound: false, verified: false, reason: "r" }),
    ).toContain("归属未验证");
  });
});

// ---------------------------------------------------------------------------
// 渲染层
// ---------------------------------------------------------------------------

describe("task-181 · A21：不可信时不得出现「本机 · 」", () => {
  it("`trusted === false` ⇒ 只有「未验证的出口」+ 原因，**没有**「本机 · 」", async () => {
    const text = await renderGlobe(
      globe({ trusted: false, verified: true, selfReason: "读不到物理默认路由 ⇒ 隧道开着时查到的是节点出口" }),
    );
    expect(text).toContain("未验证的出口");
    expect(text).toContain("隧道开着时查到的是节点出口");
    expect(text, "不可信时不许断言「本机 · 」").not.toContain("本机 · ");
    expect(text).not.toContain("本机出口");
  });

  it("`trusted === true` ⇒ 「本机出口」+ 按接口直查的说明（不夸大成「你的公网 IP 一定如此」）", async () => {
    const text = await renderGlobe(globe({ trusted: true, verified: true }));
    expect(text).toContain("本机出口");
    expect(text).toContain("按物理网卡 en0 直查");
    expect(text, "多出口时可能不同，必须写出来").toContain("可能不同");
    expect(text).not.toContain("未验证的出口");
  });

  it("`ip === null` ⇒ 「本机位置未知」", async () => {
    const text = await renderGlobe(globe({ trusted: false, verified: true, selfIp: null }));
    expect(text).toContain("本机位置未知");
    expect(text).not.toContain("本机 · ");
  });
});

describe("task-181 · A20：归属决定数字挂谁名下", () => {
  it("`verified && is_node_outbound` ⇒ 才说「节点出站累计」并显示数字", async () => {
    const text = await renderGlobe(globe({ trusted: true, verified: true, isNode: true, tag: "node-n1" }));
    expect(text).toContain("节点出站累计（出站 node-n1）");
    expect(text).toContain("9.17 GiB");
  });

  it("`verified && !is_node_outbound`（例如 direct）⇒ 如实说是该 tag 的流量，不许挂在节点名下", async () => {
    const text = await renderGlobe(
      globe({ trusted: true, verified: true, isNode: false, tag: "direct" }),
    );
    expect(text).toContain("出站 direct 的累计流量");
    expect(text).toContain("不是节点出站");
    expect(text).not.toContain("节点出站累计");
    expect(text, "数字不许挂在节点名下").not.toContain("出口 · Xray-45.207.197.185");
  });

  it("`verified === false` ⇒ **不显示数字** + 展示后端给的原因 + 不出现「出口 · <节点名>」", async () => {
    const text = await renderGlobe(
      globe({ trusted: true, verified: false, trafficReason: "查统计失败（核心没在跑或 API 不可达）⇒ 归属未验证" }),
    );
    expect(text).toContain("出口流量归属未验证（不显示数字）");
    expect(text).toContain("查统计失败");
    expect(text, "占位值一个数字都不许显示").not.toContain("9.17 GiB");
    expect(text, "归属未验证时不许出现断言式「出口 · <节点名>」").not.toContain(
      "出口 · Xray-45.207.197.185",
    );
    // 几何端点仍然标明是哪个节点（换了个名字，避免被读成「数字归它」）
    expect(text).toContain("出口节点 · Xray-45.207.197.185");
  });
});
