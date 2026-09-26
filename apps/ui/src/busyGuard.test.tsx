/**
 * task-23 C1：忙碌时顶栏「连接」/模式按钮不许「点了没反应」。
 *
 * # 原来用户会看到什么错的
 *
 * 正在「测试延迟 / 保存设置 / 更新订阅」时，顶栏那颗「连接」和三个模式按钮
 * **都是可点的**；点下去 `store.run()` 在 `busyRef.current` 检查处直接
 * `return false` —— 界面既不报错、也不变化、也不调用后端。用户得到的是
 * 「这个按钮坏了」。
 *
 * # 判据必须是 **UI 层**
 *
 * `api.start` 改前改后**都不会**被调用（`run()` 在 busy 竞态里就返回了），
 * 所以拿它当断言等于什么都没测。这里断言的是用户真正能看到的两件事：
 * 按钮 `disabled`、以及一句「正在忙什么」的 `role="status"`。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  testLatency: vi.fn(),
  selectNode: vi.fn(),
  deleteNode: vi.fn(),
  exportNode: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  incidentAnomalyCount: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      testLatency: mocks.testLatency,
      selectNode: mocks.selectNode,
      deleteNode: mocks.deleteNode,
      exportNode: mocks.exportNode,
      start: mocks.start,
      stop: mocks.stop,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
    },
    subscribe: () => () => {},
  };
});

import { TopBar } from "./App";
import Nodes from "./pages/Nodes";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider, busyLabelOf } from "./store";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
});

describe("busyLabelOf：内部操作名 → 人话（没映射到的不编具体业务名）", () => {
  it("已知操作有具体说明；未知操作只说「有操作正在进行」", () => {
    expect(busyLabelOf("probe")).toBe("正在测试延迟…");
    expect(busyLabelOf("save-rules")).toBe("正在保存规则…");
    expect(busyLabelOf("some-new-op")).toBe("有操作正在进行…");
    expect(busyLabelOf(null)).toBeNull();
  });
});

describe("忙碌时顶栏按钮必须禁用 + 有可见反馈", () => {
  it("测延迟进行中：「连接」与模式按钮 disabled，并出现 role=status 的「正在测试延迟…」", async () => {
    // 未决 Promise：操作一直「进行中」，正是现场「点了没反应」的那几秒。
    mocks.testLatency.mockReturnValue(new Promise(() => {}));
    // 顶栏那颗按钮的文案由**真实运行状态**决定：核心在跑时写「断开」，没跑时才写
    // 「连接」（`App.tsx` 里 `running ? "断开" : "连接"`）。这条验的是「忙碌时点不动
    // 连接」，所以先把快照置成未连接 —— 默认的 `scenarioSnapshot()` 是 `running: true`，
    // 那种状态下根本没有「连接」按钮可禁用。
    const base = scenarioSnapshot();
    mocks.snapshot.mockResolvedValue({ ...base, runtime: { ...base.runtime, running: false } });

    render(
      <StoreProvider>
        <TopBar view="nodes" />
        <Nodes />
      </StoreProvider>,
    );

    const probe = await screen.findByRole("button", { name: /测试全部延迟/ });
    // 仓库没装 jest-dom 的 matcher 扩展，所以直接读 DOM 属性（等价且不依赖扩展）。
    await waitFor(() => expect((probe as HTMLButtonElement).disabled).toBe(false));

    const connect = screen.getByRole("button", { name: "连接" });
    const tun = screen.getByRole("button", { name: "TUN 模式" });
    // 操作前：可点（证明下面那个 disabled 是这次操作造成的）。
    expect((connect as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(probe);

    await waitFor(() => expect((connect as HTMLButtonElement).disabled).toBe(true));
    expect((tun as HTMLButtonElement).disabled).toBe(true);
    // 可见反馈：不是只有「按钮突然点不动」。
    const badge = await screen.findByText("正在测试延迟…");
    expect(badge.getAttribute("role")).toBe("status");
  });
});
