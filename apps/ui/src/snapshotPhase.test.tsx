/**
 * task-23 A1：空态不许冒充错误态（loading / empty / error 必须分清）。
 *
 * # 原来用户会看到什么错的
 *
 * * `Dashboard.tsx` / `Routing.tsx` / `Settings.tsx`：读不到快照时永远停在
 *   「正在加载…」—— 用户一直等，而它**永远不会**变得可读；
 * * `Nodes.tsx` / `Subscriptions.tsx` 更糟：`snapshot?.nodes ?? []` 之后直接渲染
 *   「还没有任何节点 / 还没有订阅」—— 把**一次 IPC 故障**说成「你没有数据」，
 *   把人引向「去添加订阅」，即**给错原因**（本项目反复修过的「查不到 ≠ 没有」）。
 *
 * 现在：`store.snapshotPhase`（loading/loaded/failed）驱动一个共享占位
 * （`SnapshotState.tsx`）；只有 `loaded` 且数组为空时才允许出现「还没有…」。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  incidentAnomalyCount: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
    },
    subscribe: () => () => {},
  };
});

import Dashboard from "./pages/Dashboard";
import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

function snapWith(over: { nodes?: unknown[]; subscriptions?: unknown[] }) {
  const base = scenarioSnapshot();
  return {
    ...base,
    nodes: over.nodes ?? base.nodes,
    subscriptions: over.subscriptions ?? base.subscriptions,
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
});

describe("读不到状态（api.snapshot 抛错）", () => {
  beforeEach(() => {
    mocks.snapshot.mockRejectedValue({ message: "状态锁不可用" });
  });

  it("节点页：不许说「还没有任何节点」，要说「读不到状态」+ 原因 + 重试", async () => {
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    const banner = await screen.findByText(/读不到状态：状态锁不可用/);
    const text = document.body.textContent ?? "";
    expect(text, "把「没读到」说成「没有节点」＝给错原因").not.toContain("还没有任何节点");
    expect(text).toContain("这不等于「你还没有节点/订阅」");
    expect(within(banner.closest(".page") as HTMLElement, "重试")).toBeTruthy();
  });

  it("订阅页：同样不许说「还没有订阅」", async () => {
    render(
      <StoreProvider>
        <Subscriptions />
      </StoreProvider>,
    );
    await screen.findByText(/读不到状态/);
    expect(document.body.textContent ?? "").not.toContain("还没有订阅");
  });

  it("仪表盘：不许停在「正在加载…」（它永远不会结束）", async () => {
    render(
      <StoreProvider>
        <Dashboard onNavigate={() => {}} />
      </StoreProvider>,
    );
    await screen.findByText(/读不到状态/);
    expect(document.body.textContent ?? "").not.toContain("正在加载…");
  });

  it("「重试」真的再读一次（调既有 refresh）", async () => {
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    await screen.findByText(/读不到状态/);
    const calls = mocks.snapshot.mock.calls.length;
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    await waitFor(() => expect(mocks.snapshot.mock.calls.length).toBeGreaterThan(calls));
  });
});

describe("真的读到了、只是空的 ⇒ 才允许出现空态文案", () => {
  it("节点页：`loaded` + 空数组 ⇒ 「还没有任何节点」", async () => {
    mocks.snapshot.mockResolvedValue(snapWith({ nodes: [] }));
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    await screen.findByText(/还没有任何节点/);
    expect(document.body.textContent ?? "").not.toContain("读不到状态");
  });

  it("订阅页：`loaded` + 空数组 ⇒ 「还没有订阅」", async () => {
    mocks.snapshot.mockResolvedValue(snapWith({ subscriptions: [] }));
    render(
      <StoreProvider>
        <Subscriptions />
      </StoreProvider>,
    );
    await screen.findByText(/还没有订阅/);
    expect(document.body.textContent ?? "").not.toContain("读不到状态");
  });
});

/** 在一个容器里按文本找元素（本例只是为让断言读起来更直白）。 */
function within(container: HTMLElement, text: string): HTMLElement | null {
  return Array.from(container.querySelectorAll("button")).find((b) => b.textContent === text) ?? null;
}
