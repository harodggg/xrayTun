/**
 * 0.9 A1 收口：**命令失败后必须再取一次快照**，否则后端刚写下的持久陈述看不见。
 *
 * # 场景（dev-backend 的诚实修复）
 *
 * `stop_proxy` 回滚失败时后端写一条持久 `last_notice`：
 * 「未能确认网络已恢复（helper 上的会话可能仍在…）」。
 * 但命令失败只走 `fail()`（写命令错误），**不触发快照重取** ⇒ 那次失败当下
 * 用户只会看到一条瞬时命令错误，那句诚实文案要等下一次刷新才出现。
 *
 * # 这一组钉住什么
 *
 * 1. 命令失败 ⇒ `snapshot` **被再取一次**（调用计数），且**原错误仍然呈现**；
 * 2. 反例：重取**失败**时不许把原错误覆盖成另一种错误（原错误优先）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: { snapshot: mocks.snapshot, tailLogs: mocks.tailLogs },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  subscribe: () => () => {},
}));

import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider, useStore } from "./store";

/** 一个最小宿主：按一下就执行一次**必定失败**的命令，并把 `error` 显示出来。 */
function Harness() {
  const { run, error } = useStore();
  return (
    <div>
      <button
        onClick={() =>
          void run("probe", async () => {
            throw new Error("命令失败：未能确认网络已恢复");
          })
        }
      >
        失败一次
      </button>
      <span data-testid="err">{error ?? ""}</span>
    </div>
  );
}

const errText = () => screen.getByTestId("err").textContent ?? "";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.tailLogs.mockResolvedValue([]);
});

describe("0.9 A1 · 命令失败后补取一次快照", () => {
  it("失败 ⇒ snapshot 再取一次，且原错误仍然呈现", async () => {
    mocks.snapshot.mockResolvedValue(scenarioSnapshot());
    render(
      <StoreProvider>
        <Harness />
      </StoreProvider>,
    );
    // 挂载时那次初始读取
    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "失败一次" }));

    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalledTimes(2));
    expect(errText(), "命令错误必须还在（补取不是掩盖失败）").toContain(
      "命令失败：未能确认网络已恢复",
    );
  });

  it("反例：重取快照失败 ⇒ **不许**把原错误覆盖成「快照读不到」", async () => {
    // 第一次（挂载）成功，之后一律失败 —— 正好造出「重取失败」。
    mocks.snapshot.mockResolvedValueOnce(scenarioSnapshot());
    mocks.snapshot.mockRejectedValue(new Error("快照读取失败（传输层）"));
    render(
      <StoreProvider>
        <Harness />
      </StoreProvider>,
    );
    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "失败一次" }));

    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalledTimes(2));
    expect(errText(), "原错误优先").toContain("命令失败：未能确认网络已恢复");
    expect(errText(), "重取失败把原错误覆盖了").not.toContain("快照读取失败");
  });
});
