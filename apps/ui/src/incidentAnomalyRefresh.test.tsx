/**
 * task-149：哨兵角标的刷新时机。
 *
 * # Lead 裁定的四条（本文件逐条钉住）
 *
 * | # | 时机 | 对应用例 |
 * |---|---|---|
 * | ① | 挂载 | `挂载即读一次` |
 * | ② | `visibilitychange` 变可见 / 窗口 `focus` | `变可见后重新读` / `focus 也重新读` |
 * | ③ | 上传成功后 | （`task-131` 的用例已覆盖，本文件不重复） |
 * | ④ | **可见时**每 30 秒一次 | `可见时每 30 秒读一次` |
 *
 * 并且**不可见必须停表**、**卸载清 timer**、**不许绑 2 秒快照轮询**、
 * **命令抛错 ⇒ 不显示角标且不崩**。
 *
 * 为了让「不可见时停表」这件事可证伪，这里用**假时钟**推进时间并数调用次数 ——
 * 不是「看起来没有」，而是「推进 90 秒后调用次数没变」。
 */
import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  incidentPreview: vi.fn(),
  incidentUpload: vi.fn(),
  incidentAnomalyCount: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    incidentPreview: mocks.incidentPreview,
    incidentUpload: mocks.incidentUpload,
    incidentAnomalyCount: mocks.incidentAnomalyCount,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import IncidentReport, { ANOMALY_REFRESH_MS } from "./IncidentReport";

/** jsdom 的 `visibilityState` 是只读的，按场景改写它。 */
function setVisibility(state: "visible" | "hidden") {
  Object.defineProperty(document, "visibilityState", { value: state, configurable: true });
}
const fireVisibility = () => {
  document.dispatchEvent(new Event("visibilitychange"));
};
const fireFocus = () => {
  window.dispatchEvent(new Event("focus"));
};

/** 让挂载/状态更新落地（假时钟下不用 waitFor）。 */
const flush = () => act(async () => {});

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
  setVisibility("visible");
  mocks.incidentAnomalyCount.mockResolvedValue(0);
});

afterEach(() => {
  vi.useRealTimers();
});

describe("task-149 · 哨兵角标的刷新时机", () => {
  it("① 挂载即读一次（并且这时还没到 30 秒，不会多读）", async () => {
    render(<IncidentReport />);
    await flush();
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);
    // 明确证明它**不是** 2 秒级的轮询：推进 2 秒不该多读
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2_000);
    });
    expect(mocks.incidentAnomalyCount, "2 秒 cadence 与角标无关").toHaveBeenCalledTimes(1);
  });

  it("④ 可见时每 30 秒读一次，并把新值显示出来", async () => {
    render(<IncidentReport />);
    await flush();

    mocks.incidentAnomalyCount.mockResolvedValue(3);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS);
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(2);
    expect(screen.getByText("有 3 条待上报")).toBeTruthy();

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS * 2);
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(4);
  });

  it("② `visibilitychange` 变可见 ⇒ 立刻重读（不必等满 30 秒）", async () => {
    render(<IncidentReport />);
    await flush();
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);

    mocks.incidentAnomalyCount.mockResolvedValue(5);
    await act(async () => {
      fireVisibility();
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(2);
    expect(screen.getByText("有 5 条待上报")).toBeTruthy();
  });

  it("② 变可见后重新起表：之后再推进 30 秒仍有刷新", async () => {
    render(<IncidentReport />);
    await flush();
    await act(async () => {
      fireVisibility();
    });
    const before = mocks.incidentAnomalyCount.mock.calls.length;
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS);
    });
    expect(mocks.incidentAnomalyCount.mock.calls.length).toBe(before + 1);
  });

  it("② 窗口 `focus` 也重读一次", async () => {
    render(<IncidentReport />);
    await flush();
    mocks.incidentAnomalyCount.mockResolvedValue(7);
    await act(async () => {
      fireFocus();
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(2);
    expect(screen.getByText("有 7 条待上报")).toBeTruthy();
  });

  it("**不可见时停表**：推进 90 秒调用次数一次都不涨", async () => {
    render(<IncidentReport />);
    await flush();
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);

    setVisibility("hidden");
    await act(async () => {
      fireVisibility();
    });
    // 变隐藏**本身不读**（没有新信息可显示），关键是下面：表也停了
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS * 3);
    });
    expect(
      mocks.incidentAnomalyCount,
      "不可见时不该有任何后台 timer 在跑",
    ).toHaveBeenCalledTimes(1);
  });

  it("不可见 → 再变可见：恢复刷新（停表不是「永久停掉」）", async () => {
    render(<IncidentReport />);
    await flush();
    setVisibility("hidden");
    await act(async () => {
      fireVisibility();
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS * 2);
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);

    setVisibility("visible");
    await act(async () => {
      fireVisibility();
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS);
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(3);
  });

  it("卸载后不再读（timer 与监听都清掉，不留 act 噪声）", async () => {
    const { unmount } = render(<IncidentReport />);
    await flush();
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);

    unmount();
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS * 3);
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);
    // 卸载后再来事件也不该读
    await act(async () => {
      fireVisibility();
      fireFocus();
    });
    expect(mocks.incidentAnomalyCount).toHaveBeenCalledTimes(1);
  });

  it("命令抛错 ⇒ 不显示角标、整页不崩（沿用 try/catch 口径）", async () => {
    mocks.incidentAnomalyCount.mockRejectedValue(new Error("no such command"));
    render(<IncidentReport />);
    await flush();
    expect(screen.getByRole("button", { name: "报告问题" })).toBeTruthy();
    expect(screen.queryByText(/条待上报/)).toBeNull();
    // 抛错之后定时器照旧工作（下一次读到值就显示）
    mocks.incidentAnomalyCount.mockResolvedValue(2);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(ANOMALY_REFRESH_MS);
    });
    expect(screen.getByText("有 2 条待上报")).toBeTruthy();
  });
});
