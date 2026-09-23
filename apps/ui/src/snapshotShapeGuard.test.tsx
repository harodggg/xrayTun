/**
 * task-146：`store.tsx` 的**命令返回值形状守卫**。
 *
 * # 这条守卫防的故障（v0.8.35 冻结门禁的真红）
 *
 * `store.tsx` 有两条口径：**事件载荷**是校验的（`eventGuards.ts`），
 * **命令返回值**原来零校验 —— `setSnapshot(await action())` 里 `AppSnapshot`
 * 只是编译期断言，运行时不保证任何东西。坏形状因此在**各个页面**炸：
 * 全仓 `snapshot.settings.` / `snapshot.runtime.` 的**硬解引用共 78 处**
 * （Settings 67 / Routing 5 / App 4），而 `snapshot?.settings.x` 的 `?.`
 * 只挡 `snapshot === null`，**挡不住 `settings` 缺失**。
 * 后果以 **unhandled error** 出现 ⇒ **绕过通过数**（`296 passed` + `Errors 1 error`）。
 *
 * 这一组钉住：坏形状 ⇒ **保留上一份快照** + 可见告知 + 不崩；好形状 ⇒ 行为不变。
 */
import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  /** 捕获 subscribe 的回调，测试里手动触发事件（走 `refresh()` 那条路径）。 */
  handlers: null as null | Record<string, (...a: unknown[]) => void>,
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: (h: Record<string, (...a: unknown[]) => void>) => {
    mocks.handlers = h;
    return () => {};
  },
}));

import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider, useStore } from "./store";
import { resetWarnings } from "./eventGuards";

/** 探针：把 store 的三个关键值暴露成可断言的东西。 */
function Probe() {
  const { snapshot, error, run } = useStore();
  return (
    <div>
      <span data-testid="err">{error ?? ""}</span>
      <span data-testid="selected">{snapshot ? snapshot.settings.selected_node : "none"}</span>
      <span data-testid="rev">{snapshot ? snapshot.app_version : "none"}</span>
      <button onClick={() => void run("probe", () => mocks.saveSettings())}>run</button>
    </div>
  );
}

const GOOD = scenarioSnapshot();
const warnSpy = vi.spyOn(console, "error").mockImplementation(() => {});

async function mount() {
  mocks.snapshot.mockResolvedValue(GOOD);
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Probe />
    </StoreProvider>,
  );
  await waitFor(() => expect(screen.getByTestId("selected").textContent).toBe("n-hk-1"));
}

const clickRun = async () => {
  act(() => {
    screen.getByRole("button", { name: "run" }).click();
  });
};

beforeEach(() => {
  vi.clearAllMocks();
  resetWarnings();
  mocks.handlers = null;
});

describe("task-146 · 命令返回坏形状 ⇒ 保留上一份快照 + 可见告知", () => {
  it("返回 `{}` ⇒ 不崩、保留上一份快照、横幅与 console 都说出真实原因", async () => {
    await mount();
    mocks.saveSettings.mockResolvedValue({});
    await clickRun();

    await waitFor(() => expect(screen.getByTestId("err").textContent).toContain("形状异常"));
    // **保留上一份**：selected_node 仍是上一份快照里的，不是 undefined/坏值
    expect(screen.getByTestId("selected").textContent).toBe("n-hk-1");
    // 不许静默：控制台也留了痕
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining("形状异常"),
      expect.anything(),
    );
  });

  it("缺 `settings`（只给 runtime）⇒ 同样保留 + 告知", async () => {
    await mount();
    mocks.saveSettings.mockResolvedValue({ runtime: { running: true } });
    await clickRun();
    await waitFor(() => expect(screen.getByTestId("err").textContent).toContain("形状异常"));
    expect(screen.getByTestId("selected").textContent).toBe("n-hk-1");
  });

  it("反例：返回**正常快照** ⇒ 行为与今天一致（换上新数据、没有横幅）", async () => {
    await mount();
    mocks.saveSettings.mockResolvedValue({ ...GOOD, app_version: "9.9.9" });
    await clickRun();
    await waitFor(() => expect(screen.getByTestId("rev").textContent).toBe("9.9.9"));
    expect(screen.getByTestId("err").textContent).toBe("");
  });

  it("`refresh()` 那条路（事件触发重拉）同样被守卫：坏形状不覆盖上一份", async () => {
    await mount();
    mocks.snapshot.mockResolvedValue({});
    act(() => {
      mocks.handlers?.onNodesChanged?.();
    });
    await waitFor(() => expect(screen.getByTestId("err").textContent).toContain("形状异常"));
    expect(screen.getByTestId("selected").textContent).toBe("n-hk-1");
  });

  it("恢复成好形状之后，横幅被清掉（不是永久报警）", async () => {
    await mount();
    mocks.saveSettings.mockResolvedValue({});
    await clickRun();
    await waitFor(() => expect(screen.getByTestId("err").textContent).toContain("形状异常"));

    mocks.saveSettings.mockResolvedValue({ ...GOOD, app_version: "1.2.3" });
    await clickRun();
    await waitFor(() => expect(screen.getByTestId("rev").textContent).toBe("1.2.3"));
    expect(screen.getByTestId("err").textContent).toBe("");
  });
});
