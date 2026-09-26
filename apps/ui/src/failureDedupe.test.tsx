/**
 * task-23 D1 + D2：同一条失败只说一遍；失败横幅必须是 live region。
 *
 * # 原来用户会看到什么错的
 *
 * * **D1（重复）**：连接失败后回到仪表盘，同一段红字出现**两次** ——
 *   内容区顶端的全局横幅（原因 + 下一步 + 3 个动作 + 可关闭）与状态区下面那条
 *   仪表盘 notice（同样原文，但**只有 1 个动作**、且关不掉）。两条动作集不同，
 *   用户会以为发生了两次不同的故障。判据其实两端都有：命令失败的 `error` 与
 *   `runtime.last_error` 是同一句话。
 * * **D2（读屏）**：全局失败横幅**没有** `role`，所以读屏用户点「连接」失败后
 *   **什么都听不到**；而日志页/规则页的横幅早就有 `role="alert"`（同一产品两套标准）。
 *   「已自动恢复连接」也没有 `role="status"`。
 */
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  incidentAnomalyCount: vi.fn(),
}));

const sub = vi.hoisted(() => ({
  handlers: null as null | { onRuntime?: (payload: unknown) => void },
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      start: mocks.start,
      stop: mocks.stop,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
    },
    subscribe: (h: { onRuntime?: (payload: unknown) => void }) => {
      sub.handlers = h;
      return () => {};
    },
  };
});

import App from "./App";
import { scenarioSnapshot } from "./previewSnapshot";

const GATE =
  "接管默认路由之前就联系不上代理服务器 198.51.100.7:443（第1次失败、第2次失败，每次 4 秒）。\n" +
  "本次启动已中止并回滚，**默认路由没有被接管**。";

function snapWithRuntime(over: Record<string, unknown>) {
  const base = scenarioSnapshot();
  return { ...base, runtime: { ...base.runtime, ...over } };
}

beforeEach(() => {
  vi.clearAllMocks();
  sub.handlers = null;
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  mocks.stop.mockResolvedValue(scenarioSnapshot());
});

/**
 * 事件载荷里的 `traffic` 必须是**完整形状**：仪表盘会读 `rx_rate` / `rx_bytes`
 * （缺字段会在渲染时炸在 `.toFixed()` 上，而不是静默降级）。
 */
const ZERO_TRAFFIC = { rx_bytes: 0, tx_bytes: 0, rx_rate: 0, tx_rate: 0 };

describe("D1：命令失败就是 `runtime.last_error` 时，两处只留一处", () => {
  it("同一条失败 ⇒ `.banner--error .banner__reason` 恰好 1 条、`role=alert` 不超过 1 条", async () => {
    mocks.snapshot.mockResolvedValue(
      snapWithRuntime({ running: false, last_error: GATE, recovery: null }),
    );
    mocks.start.mockRejectedValue(GATE);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    // 等失败横幅出现。**不能用 `findByText(/联系不上代理服务器/)`**：这段原文在
    // 同屏至少三处命中（全局 `.banner__reason`、顶栏 `role=status` 的 sr-only
    // live region、状态区副标题），单数查询会以「Found multiple elements」误报，
    // 掩盖掉真正要测的「两条横幅说同一句话」。这里直接等目标判据本身。
    await waitFor(() =>
      expect(
        document.querySelectorAll(".banner--error .banner__reason").length,
      ).toBeGreaterThan(0),
    );

    expect(
      document.querySelectorAll(".banner--error .banner__reason").length,
      "同一条失败被说了两遍",
    ).toBe(1);
    expect(document.querySelectorAll('[role="alert"]').length).toBeLessThanOrEqual(1);
  });

  it("反例：命令错误与 `last_error` 不是同一句 ⇒ 两条都要留着（不许一律吞掉）", async () => {
    mocks.snapshot.mockResolvedValue(
      snapWithRuntime({ running: false, last_error: "上次运行出错：另一条历史错误", recovery: null }),
    );
    mocks.start.mockRejectedValue(GATE);
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    // 同上：先等横幅出现，再数条数（这里期望两条**不同**的失败并存）。
    await waitFor(() =>
      expect(
        document.querySelectorAll(".banner--error .banner__reason").length,
      ).toBeGreaterThan(0),
    );
    // 全局横幅（本次失败）+ 仪表盘（历史失败）—— 两条不同的事，都必须看得见。
    await waitFor(() =>
      expect(document.querySelectorAll(".banner--error .banner__reason").length).toBe(2),
    );
  });
});

describe("D2：失败是 alert、完成是 status", () => {
  it("全局失败横幅带 `role=\"alert\"`", async () => {
    mocks.snapshot.mockResolvedValue(snapWithRuntime({ running: false, last_error: null }));
    mocks.start.mockRejectedValue("节点连接超时：拿不到响应");
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    const reason = await screen.findByText(/节点连接超时/);
    expect(reason.closest(".banner")?.getAttribute("role")).toBe("alert");
  });

  it("「已自动恢复连接」带 `role=\"status\"`（原来完全没有 role，读屏听不到）", async () => {
    mocks.snapshot.mockResolvedValue(scenarioSnapshot());
    render(<App />);
    await waitFor(() => expect(sub.handlers?.onRuntime).toBeTruthy());

    // 事件载荷里的 `runtime` 与 `traffic` 同理，也必须是**完整形状**：store 会
    // **整体替换** `snapshot.runtime`（`store.tsx` 的 `onRuntime`），而仪表盘/顶栏
    // 随后会读 `last_error`、`running` 等字段 —— 缺字段会在渲染时炸
    // （`stripMarkup(undefined)`），不是静默降级。这里以真实快照的 runtime 为底，
    // 只覆盖这一条要验的 `recovery`。
    const baseRuntime = scenarioSnapshot().runtime;
    const runtime = (recovering: boolean, lastOutcome: string | null) => ({
      runtime: {
        ...baseRuntime,
        recovery: {
          recovering,
          attempt: 2,
          probe_failures: 0,
          started_unix: 1,
          last_outcome: lastOutcome,
          finished_unix: null,
        },
      },
      traffic: ZERO_TRAFFIC,
    });

    act(() => {
      sub.handlers!.onRuntime!(runtime(true, null));
    });
    act(() => {
      sub.handlers!.onRuntime!(runtime(false, "recovered"));
    });

    const banner = await screen.findByText(/已自动恢复连接/);
    expect(banner.closest(".banner")?.getAttribute("role")).toBe("status");
  });
});
