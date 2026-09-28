/**
 * 0.9.0 · F2 拓扑页首屏重排（PRD P0-2）+ P1-1 摘除 + F3 拓扑页 Esc（task-8，dev-ia 范围）。
 *
 * # 这一页现在的顺序（就是被断言的东西）
 *
 * ```
 * 网络流动（标题 + 一行说明）        ← 只占几十 px
 * 最近连接（真实发生的连接列表）      ← 首行 .conn-row 在这里
 *   └ 选中的那条连接的说明（.note）
 * 车流图（口径说明 → 流量不可用说明 → 图）
 * 去「分流」页判定（P1-1 的唯一入口）
 * ```
 *
 * # 关键：那条 px 判据怎么在 jsdom 里量
 *
 * PRD §6 的判据是「`.conn-row` 的 `top - content.top < 636`（改前 **1193**）」，
 * 而 jsdom **没有布局引擎**（`getBoundingClientRect` 全 0）。照
 * `topologyAnimation.test.ts` 的既有做法：装一个**显式的假布局模型**，把 DOM 顺序
 * 折算成纵向像素。模型常量全部取自仓库内的实测来源：
 *
 * | 常量 | 值 | 来源 |
 * |---|---|---|
 * | `.content` 可视高 | 636px | `docs/design/DESIGN-SYSTEM.md`（1080×720 实测） |
 * | `.page` 块间距 | 26px | `styles.css` `.page { gap: 26px }` |
 * | `.page__sec` 内间距 / 段间距 | 10 / 25px | `styles.css` `.page__sec` + `.page__sec + .page__sec` |
 * | 说明行高 | 21px | `.page__desc` 12px × 1.75 / `.note` 同 |
 * | `.note` 内边距 | 24px | `.note { padding: 12px 14px }` |
 * | 每行字符数 | 68 | `.page__desc { max-width: 68ch }`（**保守**：真实一行更长 ⇒ 估出的 top 偏大） |
 * | `.conn-filter` / `.conn-scope` / `.conn-summary` | 48 / 46 / 22px | 这三块的 padding+字体（`styles.css:2116-2167`） |
 * | `.highway` | 320px | `topologyAnimation.test.ts` 的假布局用的同一个高度 |
 *
 * ⚠️ **这是假布局证据，不是真机证据**：它证明的是「DOM 顺序 + 块高预算」，
 * 真机 macOS 的像素仍需人点一遍（0.9-PLAN §4 已写明本机没有 macOS）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  routingTopology: vi.fn(),
  recentConnections: vi.fn(),
  explainDest: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    routingTopology: mocks.routingTopology,
    recentConnections: mocks.recentConnections,
    explainDest: mocks.explainDest,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Topology from "./pages/Topology";
import { connectionsScenario } from "./previewConnections";
import { topologyScenario } from "./previewTopology";
import type { Topology as TopologyData } from "./types";

/** jsdom 没有 `ResizeObserver`；缺了它会整棵子树被 React 丢掉（见 topologyStatements）。 */
class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
globalThis.ResizeObserver = NoopResizeObserver as unknown as typeof ResizeObserver;

async function renderTopology(over: Partial<TopologyData> = {}) {
  mocks.routingTopology.mockResolvedValue({ ...topologyScenario(), ...over });
  mocks.recentConnections.mockResolvedValue(connectionsScenario());
  const onNavigate = vi.fn();
  render(<Topology onNavigate={onNavigate} />);
  // 页面真的落地：车流图的标题是它恒有的部分。
  await screen.findByText("车流图");
  return onNavigate;
}

/**
 * 最小假锚点：`Flow` 在**入口/出口卡的 rect 全为 0** 时会判定「还没布局」并
 * 直接 `setGeo(routes: [])`（`Flow.tsx:189-192`）—— jsdom 的 rect 恒为 0，
 * 所以默认情况下根本不会画 guide 路径。要断言「图与车还在」就得给锚点假坐标，
 * 做法与 `topologyAnimation.test.ts` 的 `rectOf` 同款（这里只保留最小形状）。
 */
function installFakeAnchors(): () => void {
  const real = Element.prototype.getBoundingClientRect;
  const mk = (left: number, top: number, width: number, height: number): DOMRect =>
    ({
      x: left,
      y: top,
      left,
      top,
      right: left + width,
      bottom: top + height,
      width,
      height,
      toJSON: () => ({}),
    }) as DOMRect;
  Element.prototype.getBoundingClientRect = function (this: Element): DOMRect {
    if (!this.isConnected) return mk(0, 0, 0, 0);
    if (this.classList.contains("highway")) return mk(0, 0, 820, 320);
    if (this.classList.contains("highway__lane-label")) {
      const side = this.closest(".highway__side");
      const rows = side
        ? [...side.children].filter((c) => c.classList.contains("highway__lane-label"))
        : [];
      const idx = Math.max(0, rows.indexOf(this));
      const right = side?.classList.contains("highway__side--right") ?? false;
      return mk(right ? 820 - 168 : 0, 40 + idx * 50, 168, 44);
    }
    return real.call(this);
  };
  return () => {
    Element.prototype.getBoundingClientRect = real;
  };
}

beforeEach(() => {
  vi.clearAllMocks();
});

// ---------------------------------------------------------------------------
// 假布局模型
// ---------------------------------------------------------------------------

const VISIBLE_HEIGHT = 636; // PRD §6 / DESIGN-SYSTEM：1080×720 下 .content 可视高

const M = {
  pageGap: 26,
  secGap: 10,
  secSep: 25,
  title: 21,
  line: 21,
  notePad: 24,
  listPad: 8,
  filter: 48,
  scope: 46,
  summary: 22,
  highway: 320,
  /** `.page__desc { max-width: 68ch }` —— 保守取小 ⇒ 行数偏多 ⇒ top 偏大。 */
  charsPerLine: 68,
};

function textLines(el: Element): number {
  const text = (el.textContent ?? "").replace(/\s+/g, " ").trim();
  if (!text) return 0;
  return Math.max(1, Math.ceil(text.length / M.charsPerLine));
}

/** 一个块的估高（已知块用实测值；容器递归累加子块 + 间距）。 */
function estimate(el: Element): number {
  const cls = el.classList;
  if (cls.contains("highway")) return M.highway;
  if (cls.contains("conn-filter")) return M.filter;
  if (cls.contains("conn-scope")) return M.scope;
  if (cls.contains("conn-summary")) return M.summary;
  if (cls.contains("page__title")) return M.title;
  if (cls.contains("page__desc")) return textLines(el) * M.line;
  if (cls.contains("note")) return textLines(el) * M.line + M.notePad;

  const kids = [...el.children];
  if (kids.length === 0) return 0;
  const gap = cls.contains("page") ? M.pageGap : cls.contains("page__sec") ? M.secGap : 0;
  const inner = kids.reduce((sum, k) => sum + estimate(k), 0) + gap * (kids.length - 1);
  return inner + (cls.contains("page__sec") ? M.secSep : 0);
}

/** 元素相对 `root` 顶部的纵坐标（文档序流式堆叠）。 */
function offsetOf(target: Element, root: Element): number {
  let top = 0;
  for (let node: Element | null = target; node && node !== root; node = node.parentElement) {
    for (let sib = node.previousElementSibling; sib; sib = sib.previousElementSibling) {
      top += estimate(sib);
    }
    if (node.parentElement?.classList.contains("conn-list")) top += M.listPad;
  }
  return top;
}

function inDocumentOrder(before: Element, after: Element): boolean {
  return (before.compareDocumentPosition(after) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
}

// ---------------------------------------------------------------------------
// F2 · 真实数据提到首屏
// ---------------------------------------------------------------------------

describe("0.9.0 · F2 拓扑页首屏重排（PRD P0-2）", () => {
  it("`.conn-row` 出现在车流图**之前**（真实数据优先）", async () => {
    await renderTopology();

    const row = document.querySelector(".conn-row");
    const highway = document.querySelector(".highway");
    expect(row, "最近连接的第一行没渲染，判据不成立").not.toBeNull();
    expect(highway, "车流图不许被删").not.toBeNull();
    expect(
      inDocumentOrder(row!, highway!),
      "「最近连接」必须在车流图之前 —— 改前它在图 + 判定器 + 规则链之后（第一行 top 1193 ≈ 1.9 屏）",
    ).toBe(true);
  });

  it("几何（假布局）：`.conn-row` 的 top < 636 —— 首屏内", async () => {
    await renderTopology();

    const page = document.querySelector(".page")!;
    const row = document.querySelector(".conn-row")!;
    const highway = document.querySelector(".highway")!;
    const rowTop = offsetOf(row, page);
    const highwayTop = offsetOf(highway, page);
    // 防空壳：模型不能全是 0（否则「< 636」恒真）。
    expect(estimate(page), "假布局模型退化了（总高为 0），判据不成立").toBeGreaterThan(300);
    console.log(
      `[P0-2 假布局] .conn-row top=${rowTop}px · .highway top=${highwayTop}px · 可视高=${VISIBLE_HEIGHT}px`,
    );

    expect(rowTop, "连接列表第一行落在首屏之下（改前 1193）").toBeGreaterThan(0);
    expect(highwayTop, "车流图必须仍在真实数据之后").toBeGreaterThan(rowTop);
    expect(rowTop, `.conn-row top=${rowTop}px 必须 < ${VISIBLE_HEIGHT}px`).toBeLessThan(
      VISIBLE_HEIGHT,
    );
  });

  it("P0-2 不删图不动车：车流图、guide 路径与货车都还在", async () => {
    const restore = installFakeAnchors();
    try {
      await renderTopology();

      expect(document.querySelector(".highway"), "车流图被删了").not.toBeNull();
      await waitFor(() =>
        expect(
          document.querySelectorAll("path.flow__guide").length,
          "guide 路径没了 —— 车会飞在空白处",
        ).toBeGreaterThan(0),
      );
      // 车由最后一次可信读数驱动；`traffic_ok=true` 时必须有车（§3「明确不做」：
      // 不许把 traffic_ok=false 当成停车开关，那正是 PRD 推翻的那条建议）。
      expect(
        document.querySelectorAll("g.flow__truck").length,
        "货车没了 —— 图成了装饰",
      ).toBeGreaterThan(0);
    } finally {
      restore();
    }
  });

  it("`traffic=unavailable` 时说明 `.note` 出现在车流图**之前**（PRD S5）", async () => {
    await renderTopology({
      traffic_error: "核心未运行：读不到 StatsService，本次流量不可用",
      traffic_ok: false,
    });

    const note = [...document.querySelectorAll(".note")].find((n) =>
      (n.textContent ?? "").includes("取不到实时流量"),
    );
    const highway = document.querySelector(".highway")!;
    expect(note, "流量不可用的说明不许删").toBeTruthy();
    expect(
      inDocumentOrder(note!, highway),
      "口径说明必须在它解释的那张图**之前**（改前它在图下方）",
    ).toBe(true);
    // 模型量：说明也在首屏内、在图之前。
    const page = document.querySelector(".page")!;
    expect(offsetOf(note!, page)).toBeLessThan(offsetOf(highway, page));
    // 诚实的边界文案不能丢。
    expect(note!.textContent).toContain("不画 0 字节的假流量");
  });

  it("§3「明确不做」：过滤范围免责文案仍在（不是全量搜索）", async () => {
    await renderTopology();

    const scope = document.querySelector(".conn-scope")?.textContent ?? "";
    expect(scope, "「已取到的最近 N 条（不是全量搜索）」是刻意的诚实口径，不许删").toContain(
      "已取到的",
    );
    expect(scope).toContain("不是全量搜索");
  });
});

// ---------------------------------------------------------------------------
// P1-1 · 判定器与生效规则链搬走，只留入口
// ---------------------------------------------------------------------------

describe("0.9.0 · P1-1 拓扑页不再承担判定（搬去「分流」页）", () => {
  it("域名判定输入框与生效规则链都从拓扑页消失", async () => {
    await renderTopology();

    // DestChecker 的输入框形状：占位符是它独有的。
    expect(
      document.querySelector('input[placeholder*="www.google.com"]'),
      "「某个地址会走哪条路」的输入框应从拓扑页摘除（PRD P1-1）",
    ).toBeNull();
    expect(screen.queryByText(/某个地址会走哪条路/)).toBeNull();
    // 生效规则链（`.chain`）也不在了。
    expect(document.querySelector(".chain"), "规则链应搬去分流页").toBeNull();
    expect(document.querySelector(".chain__row")).toBeNull();
    // 摘除后拓扑页不得再调判定接口（没人输入了）。
    expect(mocks.explainDest).not.toHaveBeenCalled();
  });

  it("保留一个去「分流」页判定区的入口：onNavigate('routing','judge')", async () => {
    const onNavigate = await renderTopology();

    fireEvent.click(screen.getByRole("button", { name: "去「分流」页判定某个域名" }));
    expect(
      onNavigate,
      "用户在图上产生「这个网站走哪条路」的疑问 —— PRD P1-1 要求保留这个入口",
    ).toHaveBeenCalledWith("routing", "judge");
  });

  it("段落数从 4 降到 2（判定器段与规则链段被摘除，面板嵌在第一段里）", async () => {
    await renderTopology();

    const secs = [...document.querySelectorAll(".page .page__sec")];
    const titles = secs.map((s) => s.querySelector(":scope > .page__title")?.textContent ?? "(无标题)");
    console.log(`[P1-1 段落] ${secs.length} 段：${titles.join(" / ")}`);
    expect(
      secs.length,
      `PRD P1-1 要求拓扑页段落「从 4 段降到 2 段」；实际 ${secs.length} 段：${titles.join(" / ")}`,
    ).toBe(2);
    // 两段分别是：网络流动（含嵌套的最近连接面板）与面板自身。
    expect(titles).toContain("网络流动");
    expect(titles).toContain("最近连接");
  });
});

// ---------------------------------------------------------------------------
// F3 · Esc 关闭连接详情
// ---------------------------------------------------------------------------

describe("0.9.0 · F3 拓扑页 `Esc` 关闭连接详情（UX C5）", () => {
  it("选中一条连接后按 Esc ⇒ 详情面板关闭", async () => {
    await renderTopology();

    fireEvent.click(document.querySelector(".conn-row")!);
    expect(document.querySelector(".conn-detail"), "面板没打开，判据不成立").not.toBeNull();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() =>
      expect(document.querySelector(".conn-detail"), "Esc 必须关掉详情面板").toBeNull(),
    );
  });

  it("反例：没有选中连接时 Esc 是空操作（面板本来就不在）", async () => {
    await renderTopology();

    expect(document.querySelector(".conn-detail")).toBeNull();
    fireEvent.keyDown(window, { key: "Escape" });
    expect(document.querySelector(".conn-detail")).toBeNull();
  });
});
