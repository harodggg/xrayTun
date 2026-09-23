/**
 * 日志页「读不到 ≠ 没有」的回归测试（task-23 缺陷 A）。
 *
 * # 背景
 *
 * `store.tsx` 里原来是 `.catch(() => { /* 静默即可 *\/ })`：读失败与「真的没有日志」
 * 在界面上完全一样，而空态文案又把空列表解释成「核心还没启动过」—— 用户被告知的是
 * **错误的原因**。本项目在流量字节数、连接数、域名配对上修过三次同类问题，
 * 这是第四处，也是唯一一处「给错因」的。
 *
 * # 这几条测试钉住什么
 *
 * 1. 失败态：有原因、有可重试的动作，且**不出现**「核心还没启动过」；
 * 2. 「核心没在跑」与「核心在跑但还没输出」是两句不同的话，判据是快照的
 *    `runtime.running`（后端真实值），不是前端猜；
 * 3. 三种说法互不相同 —— 防止将来又把它们合并成一句；
 * 4. 「重试」真的再取一次（不是装饰按钮）；
 * 5. 清空失败时界面**不能**装作已清空（日志文件还在，刷新就会回来）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  clearLogs: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    clearLogs: mocks.clearLogs,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Logs from "./pages/Logs";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

/**
 * 本页只关心 `runtime.running`，但快照**仍然要给完整的**：
 * 部分形状的替身是「夹具在说谎」—— 页面今天只读这一个字段，明天多读一个就会
 * 以 unhandled error 的形式炸掉（`Errors N` 会让退出码变 1，而通过数看不出来）。
 */
function snap(running: boolean, startedAt: number | null = running ? 1_700_000_000 : null) {
  const base = scenarioSnapshot();
  // `started_at_unix` 必须**显式**表达场景：空态文案的判据是它（task-128 的 B9），
  // 而不是「字段恰好缺席」—— 原来那个部分形状的替身正是因为没写这个字段，
  // 才让「核心没在跑」蒙对了「还没启动过」这句。
  //   * 没在跑 ⇒ null（确实没启动过）
  //   * 在跑   ⇒ 给一个真实时刻（于是走「已运行、但当前没有日志」那一支）
  return { ...base, runtime: { ...base.runtime, running, started_at_unix: startedAt } } as never;
}

function renderLogs() {
  return render(
    <StoreProvider>
      <Logs />
    </StoreProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snap(true));
  mocks.clearLogs.mockResolvedValue(undefined);
});

describe("日志页：读取失败不许说成「没有日志」（task-23 A）", () => {
  it("失败态给出后端原文 + 可重试，且不出现「核心还没启动过」", async () => {
    mocks.tailLogs.mockRejectedValue(new Error("Permission denied (os error 13)"));
    renderLogs();

    const banner = await screen.findByRole("alert");
    expect(banner.textContent).toContain("读取日志失败");
    expect(banner.textContent).toContain("Permission denied (os error 13)");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
    // 核心断言：读失败与「核心没启动」必须是两回事
    expect(screen.queryByText(/核心还没启动过/)).toBeNull();
  });

  it("读取成功 + 空 + 核心没在跑 → 「核心还没启动过」", async () => {
    mocks.tailLogs.mockResolvedValue([]);
    mocks.snapshot.mockResolvedValue(snap(false));
    renderLogs();

    expect(await screen.findByText(/核心还没启动过/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  // task-128：这一格的文案从「还没有产生日志」改成如实摆出启动时刻 + 不猜原因
  // （原来是「刚启动时这样是正常的」，而「刚启动」是从 running 猜的）。
  // 断言强度不变：仍然要求它与另外两种「空」是**不同的说法**。
  it("读取成功 + 空 + 核心在跑 → 说清「当前还没有日志」，不赖核心没启动", async () => {
    mocks.tailLogs.mockResolvedValue([]);
    renderLogs();

    expect(await screen.findByText(/但当前还没有日志/)).toBeTruthy();
    expect(screen.queryByText(/核心还没启动过/)).toBeNull();
  });

  it("三种「空」的说法互不相同（防止将来又被合并成一句）", async () => {
    const texts: string[] = [];

    mocks.tailLogs.mockRejectedValueOnce(new Error("boom"));
    const a = renderLogs();
    texts.push((await screen.findByText(/这不等于「没有日志」/)).textContent ?? "");
    a.unmount();

    mocks.tailLogs.mockResolvedValue([]);
    mocks.snapshot.mockResolvedValue(snap(false));
    const b = renderLogs();
    texts.push((await screen.findByText(/核心还没启动过/)).textContent ?? "");
    b.unmount();

    mocks.snapshot.mockResolvedValue(snap(true));
    const c = renderLogs();
    texts.push((await screen.findByText(/但当前还没有日志/)).textContent ?? "");
    c.unmount();

    expect(new Set(texts).size).toBe(3);
  });

  it("「重试」真的再取一次，成功后横幅消失并换成当前状态的文案", async () => {
    mocks.tailLogs.mockRejectedValueOnce(new Error("boom"));
    renderLogs();
    await screen.findByRole("alert");

    mocks.tailLogs.mockResolvedValue([]);
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(await screen.findByText(/但当前还没有日志/)).toBeTruthy();
    expect(mocks.tailLogs).toHaveBeenCalledTimes(2);
  });

  it("清空失败：界面不能装作已清空（后端说没删掉，日志就还得在）", async () => {
    mocks.tailLogs.mockResolvedValue([
      { ts_unix: 1, source: "app", level: "info", message: "既有日志" },
    ]);
    mocks.clearLogs.mockRejectedValue(new Error("Permission denied"));
    renderLogs();

    expect(await screen.findByText("既有日志")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "清空" }));
    fireEvent.click(screen.getByRole("button", { name: "确认清空" }));

    await waitFor(() => expect(mocks.clearLogs).toHaveBeenCalledTimes(1));
    // 失败 → 不能清空界面：那条日志必须还在
    expect(screen.getByText("既有日志")).toBeTruthy();
  });
});
