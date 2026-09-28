/**
 * 0.9.0 · PRD P0-3②/③：地球仪页「真实数据优先」与「主数字换成累计流量」。
 *
 * # 钉住两个用户可感知的性质
 *
 * 1. **数据在图之前**。原来 DOM 顺序是 `画布 → note → RouteFacts`，实测事实面板 top=672、
 *    操作提示 top=641 —— 都在首屏 636px 之外，用户先看到一张大图，要滚动才读到
 *    「起点/出口/流量」。本文件断言 `.facts` 在 `.globe`（画布容器）**之前**。
 * 2. **主数字是累计流量，不是大圆距离**。全应用只有这里把距离当主数字，而距离是最不可
 *    行动的字段（PRD P0-3②）。断言「字节」落在 `.facts__km`（强调），「km」落在
 *    `.facts__km--small`（降级）。
 *
 * # 边界（诚实）
 *
 * * 这里用 jsdom，**量不到像素**：断言的是 **DOM 顺序**与**类名分工**（可离线证明的等价物），
 *   不是「top < 636」那种几何结论。几何结论需要真浏览器（仓库既有审计用 headless Chrome）。
 * * 诚实口径（`traffic.verified === false` ⇒ 主数字是 `—` + 原因）由 `globeProvenance.test.tsx`
 *   覆盖；本文件只补一条「主数字槽位仍然是同一个槽位」，防止有人把降级路径挪到次要位置。
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

import Globe from "./pages/Globe";
import { StoreProvider } from "./store";
import type { GeoLocation, GlobeData } from "./types";

const LOC = (ip: string): GeoLocation => ({
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

function globeData(verified: boolean): GlobeData {
  return {
    route: {
      from: LOC("39.144.146.165"),
      to: LOC("45.207.197.185"),
      bytes: 9_846_000_000,
      node_name: "Xray-45.207.197.185",
      traffic_ok: verified,
      counter_resets: 0,
      traffic: {
        tag: verified ? "node-n1" : null,
        is_node_outbound: verified,
        verified,
        reason: verified ? null : "查统计失败（核心没在跑）⇒ 归属未验证",
      },
    },
    origin: LOC("39.144.146.165"),
    error: null,
    self_check: {
      ip: "39.144.146.165",
      bound_interface: "en0",
      trusted: true,
      reason: null,
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
  await screen.findByText(/本机出口/);
}

/** 只比较两个元素在文档里的先后（不依赖像素）。 */
function isBefore(a: Element, b: Element): boolean {
  return Boolean(a.compareDocumentPosition(b) & Node.DOCUMENT_POSITION_FOLLOWING);
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("0.9.0 · PRD P0-3③ 数据优先", () => {
  it("事实面板在画布**之前**（首屏先读到起点/出口/流量）", async () => {
    await renderGlobe(globeData(true));

    const facts = document.querySelector(".facts");
    const canvasWrap = document.querySelector(".globe");
    expect(facts).not.toBeNull();
    expect(canvasWrap).not.toBeNull();
    expect(isBefore(facts!, canvasWrap!)).toBe(true);
  });

  it("地球仪仍在页面上且画布没有被删掉（数据优先 ≠ 砍掉图）", async () => {
    await renderGlobe(globeData(true));

    const canvas = document.querySelector("canvas.globe__canvas");
    expect(canvas).not.toBeNull();
    // 操作提示仍在画布容器里（它是画布上的可发现入口，不是被移走的正文）
    const hint = document.querySelector(".globe .globe__hint");
    expect(hint).not.toBeNull();
    expect(hint!.textContent).toContain("滚轮缩放");
  });

  it("后端报错说明（note）也在画布之前 —— 出错时不该埋在图下面", async () => {
    const data = globeData(true);
    data.error = "位置查询失败：ip-api.com 超时";
    await renderGlobe(data);

    const note = document.querySelector(".note");
    const canvasWrap = document.querySelector(".globe");
    expect(note).not.toBeNull();
    expect(isBefore(note!, canvasWrap!)).toBe(true);
  });
});

describe("0.9.0 · PRD P0-3② 主数字换成累计流量", () => {
  it("已验证：字节是主数字（`.facts__km`），距离降级（`.facts__km--small`）", async () => {
    await renderGlobe(globeData(true));

    const mid = document.querySelector(".facts__mid");
    expect(mid).not.toBeNull();

    const main = mid!.querySelectorAll(".facts__km:not(.facts__km--small)");
    expect(main).toHaveLength(1);
    // 主数字是「量」，不是「距离」
    expect(main[0]!.textContent).not.toMatch(/km$/);

    const small = mid!.querySelectorAll(".facts__km--small");
    expect(small).toHaveLength(1);
    expect(small[0]!.textContent).toMatch(/km$/);
    expect(small[0]!.textContent).toMatch(/\d/); // 距离仍然显示，只是降级
  });

  it("未验证：主数字槽位仍是同一个槽位，但内容必须是 `—` + 原因（不许拿归属不明的数字当头条）", async () => {
    await renderGlobe(globeData(false));

    const mid = document.querySelector(".facts__mid");
    const main = mid!.querySelector(".facts__km:not(.facts__km--small)");
    expect(main).not.toBeNull();
    expect(main!.textContent).toBe("—");
    expect(mid!.textContent).toContain("归属未验证");
  });
});
