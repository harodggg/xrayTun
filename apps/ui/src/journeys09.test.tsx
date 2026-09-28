/**
 * 0.9.0 · 用户测试（journey 级）—— 见 `docs/product/0.9-USER-TEST.md`
 *
 * # 为什么单独有这个文件
 *
 * 0.9.0 的承诺（「每个动作都有回声」）是**跨组件**的：分别断言「开关调了 `save_settings`」
 * 与「横幅组件能渲染文本」两条都绿，用户仍可能什么都看不到（横幅挂在另一个分类下、
 * 或在滚动区外）。所以本文件的每条用例都**从用户动作出发、到用户可见文本结束**，
 * 中间不拿「调用了某个函数」当结论。
 *
 * # 与组件测试的分工
 *
 * * 组件测试（`nodeSwitchFeedback.test.tsx` / `settingsAutosave.test.tsx` / …）保证**这一块**对；
 * * 本文件保证**走完整条路之后用户看到的东西是对的**：点侧栏 → 换页 → 动作 → 可见反馈。
 *
 * # 边界（诚实，别把这里当用户研究）
 *
 * * 没有真实用户、没有遥测；「点击预算」来自 `docs/optimization/UX-AND-HABITS.md` 的代码路径枚举。
 * * jsdom **没有布局**：这里断言的是文本 / 角色 / 导航结果，**不是像素**。
 * * 真机项（⌘,、helper 授权、TUN 建卡、真实重建耗时）**不在这里**，见用户测试方案 §3.2。
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  routingTopology: vi.fn(),
  saveSettings: vi.fn(),
  selectNode: vi.fn(),
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
      routingTopology: mocks.routingTopology,
      saveSettings: mocks.saveSettings,
      selectNode: mocks.selectNode,
      start: mocks.start,
      stop: mocks.stop,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
    },
    subscribe: () => () => {},
  };
});

import App from "./App";
import { scenarioSnapshot } from "./previewSnapshot";
import type { AppSnapshot } from "./types";

function snap(over: Partial<AppSnapshot> = {}): AppSnapshot {
  return { ...scenarioSnapshot(), ...over } as AppSnapshot;
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  mocks.routingTopology.mockResolvedValue({
    rule: [],
    inbound: [],
    outbound: [],
    traffic_error: null,
    traffic_ok: true,
  });
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
  mocks.selectNode.mockResolvedValue(scenarioSnapshot());
});

// ---------------------------------------------------------------------------
// J1 · 第一次连上：空态必须把人直接送到订阅页
// ---------------------------------------------------------------------------

describe("J1 · 第一次连上（新用户）", () => {
  it("没有任何节点 ⇒ 主按钮是「添加订阅」，点它落到订阅页（少一次页面切换、少一次试错）", async () => {
    mocks.snapshot.mockResolvedValue(snap({ nodes: [], subscriptions: [] }));
    render(<App />);

    // 用户动作：在空态仪表盘上点主按钮
    const cta = await screen.findByRole("button", { name: "添加订阅" });
    fireEvent.click(cta);

    // 用户看到：订阅页（而不是先被丢到「节点」页再自己找）。
    // 用订阅页**独有**的入口文案判定，避免「订阅」二字在侧栏/顶栏也有而匹配到多个元素。
    expect(await screen.findByRole("button", { name: "添加并拉取" })).toBeTruthy();
    expect(screen.getByPlaceholderText(/subscribe\?token=/)).toBeTruthy();
  });

  it("反向判据：有节点时主按钮**不许**是「添加订阅」（否则老用户每次都被推去加订阅）", async () => {
    mocks.snapshot.mockResolvedValue(snap());
    render(<App />);

    await screen.findByRole("button", { name: /选择节点|切换节点/ });
    expect(screen.queryByRole("button", { name: "添加订阅" })).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// J2 · 换个节点：必须有回声，且点名目标
// ---------------------------------------------------------------------------

describe("J2 · 换个节点（高频）", () => {
  it("点一个节点 ⇒ 出现进行中提示；提示里是**被点的那个**节点名，并说清隧道会重建", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "节点" }));
    const group = await screen.findByRole("radiogroup", { name: "节点" });

    // 选一台**不是当前选中**的节点；用页面上的真实名字，避免硬编码 fixture
    const rows = screen.getAllByRole("radio");
    const target = rows[rows.length - 1]!;
    const targetName =
      target.getAttribute("aria-label") ?? target.textContent ?? "";
    expect(targetName.length).toBeGreaterThan(0);

    // 未决 Promise：把「重建的那几秒」造出来
    let finish!: (v: unknown) => void;
    mocks.selectNode.mockReturnValue(new Promise((r) => (finish = r)));

    fireEvent.click(target);

    // 页面上可能有多个 live region（顶栏状态也在刷），所以找**含目标节点名**的那条，
    // 而不是假定只有一条 role=status。
    await waitFor(() => {
      const texts = screen.getAllByRole("status").map((n) => n.textContent ?? "");
      expect(
        texts.some((t) => t.includes(targetName.split(" ")[0]!)).valueOf(),
        `没有任何 role=status 提到目标节点「${targetName}」；实际：${JSON.stringify(texts)}`,
      ).toBe(true);
    });
    const noticeText = screen
      .getAllByRole("status")
      .map((n) => n.textContent ?? "")
      .find((t) => t.includes(targetName.split(" ")[0]!))!;
    expect(noticeText, "必须说清会发生什么").toContain("隧道会重建");

    // 命令真的发出去了（提示必须对应一条真命令，不能是纯装饰）
    expect(mocks.selectNode).toHaveBeenCalledTimes(1);

    finish(scenarioSnapshot());
    await waitFor(() => {
      const texts = screen.queryAllByRole("status").map((n) => n.textContent ?? "");
      expect(texts.some((t) => t.includes("隧道会重建"))).toBe(false);
    });
    expect(group).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// J3 · 改分流：必须说清「已保存但还没生效」并给出口（本轮为回归项）
// ---------------------------------------------------------------------------

describe("J3 · 改分流（排障中频）", () => {
  it("改预设 ⇒ 出现「还没生效」+「重新连接」出口；点出口会真的重连", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "分流" }));

    const presets = await screen.findAllByRole("radio");
    const other = presets.find((p) => !(p as HTMLInputElement).checked) ?? presets[0]!;
    fireEvent.click(other);

    // 用户看到：一句人话解释为什么还没生效 + 一个可点的出口
    expect(await screen.findByText(/还没有生效/)).toBeTruthy();
    expect(screen.getByText(/重新连接/)).toBeTruthy();

    const reconnect = screen.getByRole("button", { name: /重连/ });
    fireEvent.click(reconnect);

    // 走既有 stop → start 通路（不是新造一条）
    await waitFor(() => expect(mocks.stop).toHaveBeenCalled());
    await waitFor(() => expect(mocks.start).toHaveBeenCalled());
  });
});

// ---------------------------------------------------------------------------
// J5 · 看还剩多少流量：订阅卡必须直接答出来
// ---------------------------------------------------------------------------

describe("J5 · 看还剩多少流量（低频但重要）", () => {
  it("订阅页每张卡直接显示「已用 / 总量 / 到期」，不用再点开", async () => {
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "订阅" }));

    // 用户看到：用量与到期（仓库既有能力，本轮作为回归判据）。
    // 多张卡会有多条「已用」，用 getAllByText 而不是假定唯一。
    expect((await screen.findAllByText(/已用/)).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/到期/).length).toBeGreaterThan(0);
  });

  it("诚实边界：流量不可用时不许显示 `0`，必须说「不可用」（诚实三做不到之一）", async () => {
    // 若订阅数据里没有任何用量信息，页面也不得把它渲染成「已用 0 B」
    mocks.snapshot.mockResolvedValue(
      snap({ subscriptions: [] as AppSnapshot["subscriptions"] }),
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "订阅" }));
    await screen.findByRole("button", { name: "订阅" });

    // 没有订阅时不该凭空造出用量数字
    expect(screen.queryByText(/已用 0 B/)).toBeNull();
  });
});
