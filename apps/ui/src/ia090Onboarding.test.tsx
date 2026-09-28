/**
 * 0.9.0 · 信息架构与上手引导（task-8，dev-ia 范围）—— 引导、术语、深链。
 *
 * # 覆盖的四条（每条对应 0.9-PLAN §3 的一个编号）
 *
 * | 编号 | 判据 |
 * |---|---|
 * | **F1** | 仪表盘**空态**（`nodes.length === 0`）主按钮 = 「添加订阅」且落到 `subscriptions`；有节点时仍是原来的节点入口 |
 * | **C1** | 侧栏项与页内主标题同词：「分流」（`App.tsx` 的 `NAV`），且不再是「规则」 |
 * | **C8** | `helper` **首次出现**处带中文解释「特权助手，安装时需要管理员密码」（顶栏 TUN tooltip + 仪表盘诊断说明） |
 * | **P1-1/F4** | 拓扑页的判定入口落到 `?view=routing#judge`：`onNavigate("routing","judge")` ⇒ `location.hash === "#judge"`（App 级端到端） |
 *
 * # 为什么这些断言是文案/结构级而不是像素级
 *
 * 真浏览器里的 px 判据（PRD §6 的 `top < 636`）只能在有布局引擎时量；jsdom 没有
 * 布局（`getBoundingClientRect` 全 0），所以几何那条放在 `ia090Topology.test.tsx`
 * 用**显式假布局模型**量（与 `topologyAnimation.test.ts` 同款做法）。本文件的
 * 断言是 DOM 级事实：按钮文案 / 跳转目标 / 导航项词 / tooltip 内容 / URL 片段。
 *
 * ⚠️ 真机未验证：本文件证明的是**代码路径**，不是 macOS 上的实际观感。
 */
import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  routingTopology: vi.fn(),
  recentConnections: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      ...actual.api,
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      routingTopology: mocks.routingTopology,
      recentConnections: mocks.recentConnections,
    },
    subscribe: () => () => {},
  };
});

import App from "./App";
import Dashboard from "./pages/Dashboard";
import { connectionsScenario } from "./previewConnections";
import { scenarioSnapshot } from "./previewSnapshot";
import { topologyScenario } from "./previewTopology";
import { StoreProvider } from "./store";
import type { AppSnapshot } from "./types";

/**
 * jsdom 没有 `ResizeObserver`，而车流图（`topology/Flow.tsx`）用它触发重新测量。
 * 缺了它，App 级测试点进拓扑页时 React 会把整棵子树丢掉（`ReferenceError`），
 * 断言只会看到「找不到文本」。这里给一个空桩（与 `topologyStatements.test.tsx` 同款）。
 */
class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
globalThis.ResizeObserver = NoopResizeObserver as unknown as typeof ResizeObserver;

function snap(over: Partial<AppSnapshot> = {}): AppSnapshot {
  return { ...scenarioSnapshot(), ...over } as AppSnapshot;
}

async function renderDashboard(over: Partial<AppSnapshot> = {}) {
  mocks.snapshot.mockResolvedValue(snap(over));
  mocks.tailLogs.mockResolvedValue([]);
  const onNavigate = vi.fn();
  render(
    <StoreProvider>
      <Dashboard onNavigate={onNavigate} />
    </StoreProvider>,
  );
  // 等仪表盘真的落地（诊断区是它恒有的部分）。
  await screen.findByText("环境自检与诊断");
  return onNavigate;
}

async function renderApp() {
  mocks.snapshot.mockResolvedValue(snap());
  mocks.tailLogs.mockResolvedValue([]);
  mocks.routingTopology.mockResolvedValue(topologyScenario());
  mocks.recentConnections.mockResolvedValue(connectionsScenario());
  render(<App />);
  // 等首屏快照落地：侧栏「仪表盘」在任何状态下都在。
  await screen.findByRole("button", { name: /仪表盘/ });
}

beforeEach(() => {
  vi.clearAllMocks();
  window.location.hash = "";
});
afterEach(() => {
  window.location.hash = "";
});

// ---------------------------------------------------------------------------
// F1 · 仪表盘空态：把「添加订阅」放到最需要它的那一刻
// ---------------------------------------------------------------------------

describe("0.9.0 · F1 仪表盘空态引导（UX T1）", () => {
  it("没有任何节点 ⇒ 主按钮是「添加订阅」，点一下直接落到订阅页", async () => {
    const onNavigate = await renderDashboard({ nodes: [] });

    const btn = screen.getByRole("button", { name: "添加订阅" });
    fireEvent.click(btn);
    expect(
      onNavigate,
      "空态点主按钮必须直落订阅页（UX T1：省一次页面切换 + 一次试错）",
    ).toHaveBeenCalledWith("subscriptions");
  });

  it("反例：有节点且未连接时**不得**出现「添加订阅」，仍走原来的节点入口", async () => {
    const base = scenarioSnapshot();
    expect(base.nodes.length, "预览快照必须真的有节点，否则这条反例是空壳").toBeGreaterThan(0);
    // 未连接（预览默认是已连接）：未连接 + 有节点时，原来的文案是「选择节点」。
    const onNavigate = await renderDashboard({
      runtime: { ...base.runtime, running: false } as AppSnapshot["runtime"],
    });

    expect(screen.queryByRole("button", { name: "添加订阅" })).toBeNull();
    const btn = screen.getByRole("button", { name: "选择节点" });
    fireEvent.click(btn);
    expect(onNavigate).toHaveBeenCalledWith("nodes");
  });
});

// ---------------------------------------------------------------------------
// C1 · 侧栏词：规则 → 分流
// ---------------------------------------------------------------------------

describe("0.9.0 · C1 侧栏「规则」→「分流」（UX C7）", () => {
  it("侧栏项叫「分流」（与页内主标题「分流预设」同词），不再叫「规则」", async () => {
    await renderApp();

    // 正面：这一项存在且叫「分流」
    const nav = document.querySelector(".sidebar__nav");
    expect(nav, "侧栏导航没渲染").not.toBeNull();
    expect(nav!.textContent).toContain("分流");
    // 反面：旧词不在了（否则用户想改分流方式时仍会在侧栏找「规则」）
    expect(nav!.textContent).not.toContain("规则");
    expect(screen.queryByRole("button", { name: "规则" })).toBeNull();
  });

  it("点「分流」侧栏项真的进的是规则/预设页（词改了、落点没改）", async () => {
    await renderApp();

    fireEvent.click(screen.getByRole("button", { name: "分流" }));
    // 规则页恒有的东西：预设单选组（页内主标题就是「分流预设」）
    expect(await screen.findByRole("radiogroup", { name: "分流预设" })).toBeTruthy();
    expect(document.querySelector(".nav-item.is-active")?.textContent).toContain("分流");
  });
});

// ---------------------------------------------------------------------------
// C8 · helper 首次出现带中文解释
// ---------------------------------------------------------------------------

describe("0.9.0 · C8 `helper` 首次出现处必须有中文解释（UX C8）", () => {
  it("顶栏 TUN 模式按钮的 tooltip 里，`helper` 旁边就是「特权助手，安装时需要管理员密码」", async () => {
    await renderApp();

    const tun = screen.getByRole("button", { name: "TUN 模式" });
    const title = tun.getAttribute("title") ?? "";
    expect(title, "TUN 的 tooltip 里必须出现 helper 这个词").toContain("helper");
    expect(title, "helper 首次出现必须带中文解释 —— 用户不知道它是不是自己要装的东西").toContain(
      "特权助手",
    );
    expect(title).toContain("管理员密码");
  });

  it("仪表盘诊断说明里同样带解释（`helper` 的另一处出现）", async () => {
    await renderDashboard();

    // JSX 里换行会折成一个空格，所以用「整段文本 + 包含关系」判，而不是逐字相等。
    const blurb = screen.getByText(/TUN 模式需要两个外部条件/);
    expect(blurb.textContent, "仪表盘的 helper 说明必须自带翻译").toContain("helper");
    expect(blurb.textContent).toContain("特权助手，安装时需要管理员密码");
  });
});

// ---------------------------------------------------------------------------
// P1-1 · 深链：拓扑 → 分流#judge（F4 的「我范围内锚点一致性」）
// ---------------------------------------------------------------------------

describe("0.9.0 · P1-1 拓扑 → 分流页判定区的深链（`?view=routing#judge`）", () => {
  it("App 级端到端：从拓扑点判定入口 ⇒ 落在分流页且 URL 片段变成 `#judge`", async () => {
    await renderApp();

    fireEvent.click(screen.getByRole("button", { name: "拓扑" }));
    await screen.findByText(/入口是流量进来的地方/);

    fireEvent.click(screen.getByRole("button", { name: "去「分流」页判定某个域名" }));

    expect(await screen.findByRole("radiogroup", { name: "分流预设" })).toBeTruthy();
    expect(
      window.location.hash,
      "URL 契约 `?view=routing#judge` 里的锚点必须真的写进 location.hash",
    ).toBe("#judge");
    expect(document.querySelector(".nav-item.is-active")?.textContent).toContain("分流");
  });
});
