/**
 * 0.9.0 P0 独立探针（verifier-0.9 / task-9）。
 *
 * 目的：**不采信开发自述**，用验证者自己的 fixture 与断言，逐条钉住 P0 的
 * 「用户能看到什么」。文件名刻意不含产品语义：这些是验收判据，不是产品测试。
 *
 * 本轮覆盖：
 *   B1 设置页逐项自动保存 + 真撤销（载荷级别：撤销 = 把旧值再存一次）
 *   B3 换节点进行中提示带**目标节点名**，且不加二次确认
 *   D1 图例是真实线段（`<line data-legend-kind>`），渲染路径上有 `stroke-dasharray`
 *   D2 日志等级字符（颜色之外的第二编码）
 *   F1 空态主按钮 =「添加订阅」并直落订阅页
 *   C1a 侧栏「分流」（不再是「规则」）
 *   G1 无效开关 `restore_system_proxy_on_exit` 不出现在界面上
 *
 * 诚实边界：这是 jsdom（UI 证据），不是真机 macOS 证据。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// 一个 Proxy 化的 ipc mock：页面/store 需要的每个命令都得到同一个稳定 vi.fn，
// 避免逐个补齐 api 表面（漏一个就是 "not a function" 的假失败）。
const h = vi.hoisted(() => ({ fns: new Map<string, ReturnType<typeof vi.fn>>() }));

vi.mock("../ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../ipc")>();
  const api = new Proxy(
    {},
    {
      get: (_t, k: string) => {
        if (!h.fns.has(k)) h.fns.set(k, vi.fn());
        return h.fns.get(k);
      },
    },
  );
  // api 整体替掉；另外 subscribe 必须是 no-op —— 真品会去注册 Tauri 事件监听，
  // jsdom 里 window.__TAURI_INTERNALS__ 不存在，会留下 unhandled rejection。
  // 其余具名导出（recoveryView / EVENTS / errorText / …）用真品，避免漏一个导出
  // 得到与被测行为无关的假失败。
  return { ...actual, api, subscribe: () => () => {} };
});

// jsdom 没有 ResizeObserver / matchMedia —— 拓扑 Flow 会用到。
class RO {
  observe() {}
  unobserve() {}
  disconnect() {}
}
(globalThis as unknown as { ResizeObserver: unknown }).ResizeObserver ??= RO;
(globalThis as unknown as { matchMedia: unknown }).matchMedia ??= () => ({
  matches: false,
  addEventListener() {},
  removeEventListener() {},
  addListener() {},
  removeListener() {},
});

import App from "../App";
import Dashboard from "../pages/Dashboard";
import Logs from "../pages/Logs";
import Nodes from "../pages/Nodes";
import Settings from "../pages/Settings";
import Topology from "../pages/Topology";
import type { AppSnapshot } from "../types";
import { StoreProvider } from "../store";
import { scenarioSnapshot } from "../previewSnapshot";
import { topologyScenario } from "../previewTopology";

/**
 * jsdom 没有布局引擎，`Flow` 在入口/出口卡 rect 全 0 时会判定「还没布局」并
 * 直接给出空路由（`Flow.tsx` 的 geo 分支），于是**流向图根本不画**。
 * 这里给锚点最小假坐标（与 `ia090Topology.test.tsx` 同款做法），让渲染路径真的出现。
 */
function installFakeAnchors(): () => void {
  const real = Element.prototype.getBoundingClientRect;
  const mk = (left: number, top: number, width: number, height: number): DOMRect =>
    ({ x: left, y: top, left, top, right: left + width, bottom: top + height, width, height,
       toJSON: () => ({}) }) as DOMRect;
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

const fn = (name: string) => {
  if (!h.fns.has(name)) h.fns.set(name, vi.fn());
  return h.fns.get(name)!;
};
const snap = () => scenarioSnapshot() as unknown as AppSnapshot;
const callsOf = (name: string) => fn(name).mock.calls;

function renderInStore(node: React.ReactElement) {
  return render(<StoreProvider>{node}</StoreProvider>);
}

beforeEach(() => {
  for (const f of h.fns.values()) f.mockReset();
  const base = snap();
  fn("snapshot").mockResolvedValue(base);
  fn("tailLogs").mockResolvedValue([]);
  fn("saveSettings").mockImplementation(async (next: unknown) => ({
    ...snap(),
    settings: next,
  }));
  fn("selectNode").mockResolvedValue(base);
  fn("start").mockResolvedValue(base);
  fn("stop").mockResolvedValue(base);
  fn("routingTopology").mockResolvedValue(topologyScenario());
  fn("clearLogs").mockResolvedValue(undefined);
  fn("diagnostics").mockResolvedValue("（诊断）");
});

describe("B1 设置页：逐项自动保存 + 真的可撤销", () => {
  it("拨一下开关就落盘：一次改动 = 一次 save_settings，且没有「保存/放弃」", async () => {
    renderInStore(<Settings />);
    await screen.findByRole("tablist");

    expect(screen.queryByRole("button", { name: "保存" })).toBeNull();
    expect(screen.queryByRole("button", { name: "放弃" })).toBeNull();

    const box = screen.getByRole("checkbox", { name: /允许局域网设备使用本机代理/ }) as HTMLInputElement;
    const before = box.checked;
    fireEvent.click(box);

    await waitFor(() => expect(fn("saveSettings")).toHaveBeenCalledTimes(1));
    const payload = callsOf("saveSettings")[0]![0] as { allow_lan: boolean };
    expect(payload.allow_lan, "改动必须原样进载荷").toBe(!before);
  });

  it("有回声 + 撤销把**上一个值再存一次**（不是只回滚界面）", async () => {
    renderInStore(<Settings />);
    await screen.findByRole("tablist");

    const box = screen.getByRole("checkbox", { name: /允许局域网设备使用本机代理/ }) as HTMLInputElement;
    const before = box.checked;
    fireEvent.click(box);
    await waitFor(() => expect(fn("saveSettings")).toHaveBeenCalledTimes(1));

    const echo = await screen.findByRole("status");
    expect(echo.textContent).toContain("已保存");
    fireEvent.click(screen.getByRole("button", { name: "撤销" }));

    await waitFor(() => expect(fn("saveSettings")).toHaveBeenCalledTimes(2));
    const second = callsOf("saveSettings")[1]![0] as { allow_lan: boolean };
    expect(second.allow_lan, "撤销 = 旧值再存一次").toBe(before);
  });
});

describe("B3 换节点：进行中提示带目标节点名，且不加确认", () => {
  it("点另一台节点 ⇒ 出现「正在切换到「<名字>」」", async () => {
    const base = snap() as unknown as { nodes: Array<{ id: string; name: string }> };
    // 让切换一直进行中，才能稳定观察提示
    fn("selectNode").mockImplementation(() => new Promise(() => {}));
    fn("snapshot").mockResolvedValue(base);

    renderInStore(<Nodes />);
    await screen.findByRole("radiogroup", { name: "节点" });

    const rows = screen.getAllByRole("radio");
    expect(rows.length, "fixture 至少两台节点").toBeGreaterThan(1);
    const targetName = base.nodes[1]!.name;
    fireEvent.click(rows[1]!);

    await waitFor(() => {
      expect(document.body.textContent).toContain(`正在切换到「${targetName}」`);
    });
    // 高频动作：不许弹二次确认
    expect(screen.queryByText(/确定要切换/)).toBeNull();
  });
});

describe("F1 空态主按钮：添加订阅并直落订阅页", () => {
  it("零节点时按钮是「添加订阅」，点击 onNavigate('subscriptions')", async () => {
    const base = snap() as unknown as Record<string, unknown>;
    fn("snapshot").mockResolvedValue({ ...(base as object), nodes: [] } as unknown as AppSnapshot);
    const onNavigate = vi.fn();

    renderInStore(<Dashboard onNavigate={onNavigate} />);
    const btn = await screen.findByRole("button", { name: "添加订阅" });
    fireEvent.click(btn);
    expect(onNavigate).toHaveBeenCalledWith("subscriptions");
  });
});

describe("D2 日志等级：颜色之外的字符编码", () => {
  it("warn/error 行分别带 W / E 字符", async () => {
    fn("tailLogs").mockResolvedValue([
      { seq: 1, ts_unix: 1700000000, source: "core", level: "warn", message: "w" },
      { seq: 2, ts_unix: 1700000001, source: "core", level: "error", message: "e" },
    ] as never);

    const { container } = renderInStore(<Logs />);
    await waitFor(() => {
      expect(container.querySelectorAll(".log-line").length).toBe(2);
    });
    const warn = container.querySelector(".log-line--warn .log-line__ts")?.textContent ?? "";
    const err = container.querySelector(".log-line--error .log-line__ts")?.textContent ?? "";
    expect(warn.trim().startsWith("W"), `warn 行首应是 W：${warn}`).toBe(true);
    expect(err.trim().startsWith("E"), `error 行首应是 E：${err}`).toBe(true);
  });
});

describe("D1 线型第二编码：图例是真线段 + 渲染路径有 dasharray", () => {
  it("图例项内部是真实 <line>，且**流向图的分支线**带 stroke-dasharray", async () => {
    const restore = installFakeAnchors();
    try {
    const { container } = renderInStore(<Topology />);
    await waitFor(() => {
      expect(container.querySelectorAll("[data-legend-kind]").length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(container.querySelectorAll("svg.flow").length).toBeGreaterThan(0);
    });

    // ① 图例说真话：每个图例项内部是一条真实 <line>（不是纯色块）
    const legend = Array.from(container.querySelectorAll("[data-legend-kind]"));
    const segs = legend.map((el) => el.querySelector("line"));
    expect(segs.every((el) => el !== null), "每个图例项内部必须是一条真实 <line>").toBe(true);
    expect(segs.every((el) => (el?.getAttribute("stroke") ?? "").length > 0)).toBe(true);
    expect(segs.every((el) => (el?.getAttribute("stroke-width") ?? "").length > 0)).toBe(true);

    // ② 第二编码真的在**渲染路径**上：流向图的分支线（`.flow__route`）必须带线型。
    //    刻意不查全局 `[stroke-dasharray]`：图例的 <line> 和 `.flow__highlight` 也有，
    //    那会让"只改了图例"也能蒙混通过（这正是本探针第一版踩过的坑）。
    const flowDashed = Array.from(container.querySelectorAll("path.flow__route[stroke-dasharray]"));
    expect(
      flowDashed.length,
      "流向图分支线必须带线型第二编码（只改图例不算）",
    ).toBeGreaterThan(0);

    // ③ 图例承诺的线型必须在图上真实出现过
    const flowDash = new Set(flowDashed.map((el) => el.getAttribute("stroke-dasharray")));
    const legendNonSolid = segs
      .map((el) => el?.getAttribute("stroke-dasharray") ?? "none")
      .filter((d) => d !== "none");
    if (legendNonSolid.length > 0) {
      expect(
        legendNonSolid.some((d) => flowDash.has(d)),
        `图例线型 ${legendNonSolid.join(",")} 在流向图上不存在`,
      ).toBe(true);
    }
    } finally {
      restore();
    }
  });
});

describe("C1b / F2：术语人话化与拓扑首屏顺序", () => {
  it("C1b：helper 首次出现处带「特权助手，安装时需要管理员密码」", async () => {
    const app = renderInStore(<App />);
    await screen.findByRole("navigation");
    const hint = app.container.querySelector('[title*="特权助手"]');
    expect(hint, "顶栏 TUN 入口的提示里必须有「特权助手…」中文解释").not.toBeNull();
    expect(hint?.getAttribute("title") ?? "").toContain("安装时需要管理员密码");
  });

  it("C1b：哨兵标签仍可见（保持可见，只改人话）", async () => {
    renderInStore(<Settings />);
    await screen.findByRole("tablist");
    expect(document.body.textContent ?? "", "哨兵标签必须仍可见").toContain("隧道内 DNS 地址");
  });

  it("F2：拓扑首屏把「最近连接」排在「车流图」之前", async () => {
    const restore = installFakeAnchors();
    try {
      const { container } = renderInStore(<Topology />);
      await waitFor(() => expect(container.querySelectorAll(".page__title").length).toBeGreaterThan(0));
      const text = container.textContent ?? "";
      const recent = text.indexOf("最近连接");
      const flow = text.indexOf("车流图");
      expect(recent, "页面上找不到「最近连接」").toBeGreaterThanOrEqual(0);
      expect(flow, "页面上找不到「车流图」").toBeGreaterThanOrEqual(0);
      expect(recent, "真实数据必须排在车流图之前（P0-2）").toBeLessThan(flow);
      const titles = Array.from(container.querySelectorAll(".page__title")).map((e) => e.textContent ?? "");
      expect(titles[0], `第一个分节标题：${titles.join(" / ")}`).toBe("网络流动");
      expect(
        titles.indexOf("最近连接"),
        `首屏分节顺序：${titles.join(" / ")}`,
      ).toBeLessThan(titles.indexOf("车流图"));
    } finally {
      restore();
    }
  });
});

describe("C1a / G1：侧栏用词与无效开关", () => {
  it("侧栏是「分流」而不是「规则」", async () => {
    renderInStore(<App />);
    const nav = await screen.findByRole("navigation");
    expect(nav.textContent).toContain("分流");
    expect(nav.textContent).not.toContain("规则");
  });

  it("G1：界面上找不到 restore_system_proxy_on_exit 开关", async () => {
    renderInStore(<Settings />);
    await screen.findByRole("tablist");
    expect(document.body.textContent).not.toContain("restore_system_proxy_on_exit");
    expect(screen.queryByText(/退出时恢复系统代理/)).toBeNull();
  });
});
