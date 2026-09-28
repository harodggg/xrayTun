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

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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

import Globe, { locationFreshnessText, relativeAge } from "./pages/Globe";
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

function globeData(verified: boolean, cache?: GlobeData["cache"]): GlobeData {
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
    ...(cache === undefined ? {} : { cache }),
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

// ---------------------------------------------------------------------------
// 0.9.1 · 改名「地球仪」→「位置」+ 定位缓存（IP 变了才重查）
// ---------------------------------------------------------------------------

describe("0.9.1 · 页面改名为「位置」", () => {
  it("页内主标题是「位置」，且不再出现旧词「地球仪」", async () => {
    await renderGlobe(globeData(true));

    const title = document.querySelector(".page__title");
    expect(title?.textContent).toBe("位置");
    // 用户可见文本里不该再有旧名（注释/文档里的词不参与，这里只看渲染结果）
    expect(document.body.textContent).not.toContain("地球仪");
  });
});

describe("0.9.1 · 位置新鲜度必须如实写出来", () => {
  it("relativeAge：分钟 / 小时 / 天，且未来时间戳不写负值", () => {
    expect(relativeAge(1_000_000, 1_000_030)).toBe("刚刚");
    expect(relativeAge(1_000_000, 1_000_000 + 120)).toBe("2 分钟前");
    expect(relativeAge(1_000_000, 1_000_000 + 7200)).toBe("2 小时前");
    expect(relativeAge(1_000_000, 1_000_000 + 3 * 86400)).toBe("3 天前");
    expect(relativeAge(1_000_000, 999_000)).toBe("刚刚"); // 时钟回拨不出现「-1 分钟前」
    expect(relativeAge(Number.NaN, 1_000_000)).toBeNull();
  });

  it("来自缓存 ⇒ 说清「来自缓存」+ 多久以前 + IP 未变化", () => {
    const text = locationFreshnessText(
      { from_cache: true, fetched_unix: 1_000_000, ip_changed: false },
      1_000_000 + 3 * 86400,
    );
    expect(text).toContain("位置来自缓存");
    expect(text).toContain("3 天前");
    expect(text).toContain("公网 IP 未变化");
  });

  it("IP 变了 ⇒ 说「按新的公网 IP 重新查询」（与「手动重查」区分开）", () => {
    expect(
      locationFreshnessText(
        { from_cache: false, fetched_unix: null, ip_changed: true },
        1_000_000,
      ),
    ).toBe("已按新的公网 IP 重新查询");
    expect(
      locationFreshnessText(
        { from_cache: false, fetched_unix: null, ip_changed: false },
        1_000_000,
      ),
    ).toBe("已重新查询（手动触发）");
  });

  it("缺 cache 字段（旧后端/预览）⇒ 不渲染这一行，也**不许**编造「来自缓存」", async () => {
    await renderGlobe(globeData(true)); // 不带 cache
    expect(screen.queryByTestId("location-freshness")).toBeNull();
    expect(document.body.textContent).not.toContain("来自缓存");
  });

  it("有 cache 字段 ⇒ 渲染成 role=status（点重新定位后读屏能听到变化）", async () => {
    await renderGlobe(
      globeData(true, { from_cache: true, fetched_unix: 1_790_000_000, ip_changed: false }),
    );
    const line = screen.getByTestId("location-freshness");
    expect(line.getAttribute("role")).toBe("status");
    expect(line.textContent).toContain("来自缓存");
  });
});

describe("0.9.1 · 「重新定位」是强制刷新；进页面走缓存", () => {
  it("挂载时 force=false（用缓存，不强制联网）", async () => {
    await renderGlobe(globeData(true));
    expect(mocks.globeData).toHaveBeenCalledWith(false);
  });

  it("点「重新定位」⇒ force=true（忽略缓存重查一次）", async () => {
    await renderGlobe(globeData(true));
    fireEvent.click(screen.getByRole("button", { name: "重新定位" }));
    await waitFor(() => expect(mocks.globeData).toHaveBeenCalledWith(true));
  });
});

// ---------------------------------------------------------------------------
// 0.9.1 · 缓存命中率优化带来的三种「非新鲜」情形必须分开说
// ---------------------------------------------------------------------------

describe("0.9.1 · 命中率优化后的文案（stale / node-last / ipv6-prefix）", () => {
  it("stale（探测失败/刚失败过）⇒ 说「暂时无法确认公网 IP 是否变化」，**不许**说成「未变化」", () => {
    const text = locationFreshnessText(
      {
        from_cache: true,
        fetched_unix: 1_000_000,
        ip_changed: false,
        stale: true,
        probe_cached: false,
        key_kind: "ip",
        age_s: 300,
      },
      1_000_000,
    );
    expect(text).toContain("暂时无法确认公网 IP 是否变化");
    expect(text).not.toContain("公网 IP 未变化");
  });

  it("node-last（节点 IP 变了、用的是该节点上次位置）⇒ 必须点名", () => {
    const text = locationFreshnessText(
      {
        from_cache: true,
        fetched_unix: 1_000_000,
        ip_changed: true,
        stale: false,
        key_kind: "node-last",
        age_s: 7200,
      },
      1_000_000,
    );
    expect(text).toContain("该节点的上次已知位置");
    expect(text).toContain("2 小时前");
  });

  it("ipv6-prefix ⇒ 说清粒度是「网段」，不是那一台主机", () => {
    const text = locationFreshnessText(
      {
        from_cache: true,
        fetched_unix: 1_000_000,
        ip_changed: false,
        key_kind: "ipv6-prefix",
        age_s: 30,
      },
      1_000_000,
    );
    expect(text).toContain("按 IPv6 前缀匹配");
  });

  it("age_s 优先于自算：后端给了年龄就不再看 fetched_unix", () => {
    const text = locationFreshnessText(
      { from_cache: true, fetched_unix: 0, ip_changed: false, age_s: 3 * 86400 },
      0,
    );
    expect(text).toContain("3 天前");
  });

  it("缺 stale / key_kind（旧后端）⇒ 按旧口径，不编造告警", () => {
    const text = locationFreshnessText(
      { from_cache: true, fetched_unix: 1_000_000, ip_changed: false },
      1_000_000,
    );
    expect(text).toContain("公网 IP 未变化");
    expect(text).not.toContain("暂时无法确认");
  });

  it("probe_cached 不写进可见文案，但放进 title（排障可见「本次零网络请求」）", async () => {
    await renderGlobe(
      globeData(true, {
        from_cache: true,
        fetched_unix: 1_790_000_000,
        ip_changed: false,
        probe_cached: true,
        age_s: 60,
      }),
    );
    const line = screen.getByTestId("location-freshness");
    expect(line.textContent).not.toContain("探测");
    expect(line.getAttribute("title")).toContain("没有联网探测");
  });
});
