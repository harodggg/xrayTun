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
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
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

describe("D1：命令失败就是 `runtime.last_error` 时，两处只留一处", () => {
  it("同一条失败 ⇒ `.banner--error .banner__reason` 恰好 1 条、`role=alert` 不超过 1 条", async () => {
    mocks.snapshot.mockResolvedValue(
      snapWithRuntime({ running: false, last_error: GATE }),
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
      snapWithRuntime({ running: false, last_error: "上次运行出错：另一条历史错误" }),
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

describe("D2：失败横幅必须是 live region", () => {
  it("全局失败横幅带 `role=\"alert\"`", async () => {
    mocks.snapshot.mockResolvedValue(snapWithRuntime({ running: false, last_error: null }));
    mocks.start.mockRejectedValue("节点连接超时：拿不到响应");
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));
    const reason = await screen.findByText(/节点连接超时/);
    expect(reason.closest(".banner")?.getAttribute("role")).toBe("alert");
  });
});
