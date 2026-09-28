/**
 * task-23 F1：分流页未保存的草稿切页**不许静默丢光**。
 *
 * # 原来用户会看到什么错的
 *
 * 在「分流」页编了几条规则（`Routing.tsx` 的 `draft` 是**组件内 state**），
 * 切到「日志」看一眼再切回来 —— 组件卸载，草稿归零，**改动全部消失**：
 * 没有提示、没有恢复、没有确认。这是整轮 UX 走查里唯一会**直接丢掉用户工作**的问题。
 *
 * 现在的口径：分流页把 dirty 同步到 store（`hasUnsavedEdits`），App 在任何切页
 * 动作（侧栏/状态卡片的「去处理」）前拦一次，问清「留在本页」还是「放弃改动并离开」。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  routingTopology: vi.fn(),
  saveSettings: vi.fn(),
  incidentAnomalyCount: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      routingTopology: mocks.routingTopology,
      saveSettings: mocks.saveSettings,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
      start: mocks.start,
      stop: mocks.stop,
    },
    subscribe: () => () => {},
  };
});

import App from "./App";
import { scenarioSnapshot } from "./previewSnapshot";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  mocks.routingTopology.mockResolvedValue({ rule: [], inbound: [], outbound: [], traffic_error: null, traffic_ok: true });
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

/** 走到「分流」页（0.9.0 C1：侧栏项由「规则」改名，落点不变）并制造一条未保存的改动。 */
async function editOneRule() {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: "分流" }));
  await screen.findByRole("button", { name: "保存规则" });
  fireEvent.click(screen.getByRole("button", { name: "新增规则" }));
  // 草稿生效：页面自己会写「有未保存的改动。」。
  await screen.findByText("有未保存的改动。");
}

describe("F1：规则草稿切页前的二次确认", () => {
  it("有草稿时点侧栏「日志」⇒ 先问一句，不立刻切页", async () => {
    await editOneRule();
    fireEvent.click(screen.getByRole("button", { name: "日志" }));
    expect(await screen.findByText(/未保存的规则改动/)).toBeTruthy();
    // 还在分流页（日志页的 panic.log 指引没出现）。
    expect(screen.queryByText(/panic\.log/)).toBeNull();
  });

  it("「留在本页」⇒ 不切页，且草稿还在（改动没丢）", async () => {
    await editOneRule();
    fireEvent.click(screen.getByRole("button", { name: "日志" }));
    fireEvent.click(await screen.findByRole("button", { name: "留在本页" }));
    await waitFor(() => expect(screen.queryByText(/未保存的规则改动/)).toBeNull());
    expect(screen.getByRole("button", { name: "保存规则" })).toBeTruthy();
    expect(screen.getByText("有未保存的改动。")).toBeTruthy();
  });

  it("「放弃改动并离开」⇒ 才真的切到日志页", async () => {
    await editOneRule();
    fireEvent.click(screen.getByRole("button", { name: "日志" }));
    fireEvent.click(await screen.findByRole("button", { name: "放弃改动并离开" }));
    expect(await screen.findByText("logs/panic.log")).toBeTruthy();
  });

  it("反例：没有草稿时切页**不**打扰（不能变成每次切页都弹问句）", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "分流" }));
    await screen.findByRole("button", { name: "保存规则" });
    fireEvent.click(screen.getByRole("button", { name: "日志" }));
    expect(await screen.findByText("logs/panic.log")).toBeTruthy();
    expect(screen.queryByText(/未保存的规则改动/)).toBeNull();
  });
});

/**
 * C1（0.9.0 / UX C7）：侧栏项「规则」→「分流」。
 *
 * 这条改名不是纯字符串替换，判据是「**侧栏标签与页内主标题同词**」：
 * 页内主标题是「分流预设」，而「规则」在行业里通常指**自定义规则列表** ——
 * 用户想换分流方式时会在侧栏找「规则」，点进来看到的却是预设。
 */
describe("C1：侧栏「分流」与页内主标题同词", () => {
  it("侧栏项与页内主标题都以「分流」开头，且侧栏不再出现旧词「规则」", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "分流" }));
    await screen.findByRole("radiogroup", { name: "分流预设" });

    const navLabel = document.querySelector(".nav-item.is-active")?.textContent ?? "";
    const pageTitle = document.querySelector(".page__title")?.textContent ?? "";
    expect(navLabel).toContain("分流");
    expect(pageTitle).toContain("分流");
    expect(
      pageTitle.startsWith("分流"),
      `侧栏「${navLabel}」与页内主标题「${pageTitle}」不同词 —— 用户想改分流时会回侧栏试`,
    ).toBe(true);
    // 旧词必须从侧栏消失（「自定义规则」页内保留是**刻意**的，那是用户会搜的词）。
    expect(document.querySelector(".sidebar__nav")?.textContent).not.toContain("规则");
  });
});
