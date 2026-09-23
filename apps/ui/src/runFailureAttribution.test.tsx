/**
 * task-151：`run()` 返回 `false` 时，**局部文案不许替后端编原因**。
 *
 * # 判定的口径
 *
 * `run()` 的 `false` 有三个来源且**调用方无法区分**（写进了 `store.tsx` 里 `run` 的文档）：
 * ① busy 竞态（点击根本没执行）；② 命令抛错；③ 命令成功但快照形状异常（`task-146` 的守卫）。
 * 真实原因**已经在 `error` 里**、由 App 顶部横幅显示。
 *
 * # 盘点（只读，全仓 `apps/ui/src/**`）
 *
 * 消费 `run()`/`runVoid()` 布尔返回值的只有 **3 处**：
 * | 位置 | 失败时的局部文案 | 判定 |
 * |---|---|---|
 * | `Settings.tsx:302` `save()` | 无（只 `if (ok) setDraft(null)`；错误由横幅显示） | ✅ 保留 |
 * | `Subscriptions.tsx:32` `add()` | 无（只 `if (ok) { 清空输入 }`） | ✅ 保留 |
 * | `Nodes.tsx:57` `submitManual()` | 原来是「解析失败，请检查链接格式」 | ❌ **硬编码归因** ⇒ 本卡改掉 |
 *
 * 另外 `Nodes.tsx` 的导出路径用的是 `catch (e) { setExportError(errorText(e)) }`
 * （真实原因），`DestChecker` 的 `setErr(errorText(e))` 同理 —— 都不是断言性文案。
 *
 * # 本文件钉住什么
 *
 * * 后端**报错**（真实原因在横幅）与后端**返回坏形状**（原因也在横幅）两种情形下，
 *   局部文案都是**同一句中性话**，且**不含**任何具体归因；
 * * 反例：添加成功 ⇒ 局部文案根本不出现（且输入被清空）；
 * * 这句话必须**指向权威来源**（顶部提示条），而不是让用户自己去猜。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  addManualNode: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    addManualNode: mocks.addManualNode,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Nodes from "./pages/Nodes";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

async function renderAndSubmit() {
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Nodes />
    </StoreProvider>,
  );
  fireEvent.click(await screen.findByRole("button", { name: "手动添加" }));
  fireEvent.change(screen.getByPlaceholderText(/三种都行/), {
    target: { value: "ssr://example.com" },
  });
  fireEvent.click(screen.getByRole("button", { name: "解析并添加" }));
}

/** 局部错误块（`banner--error` 里那段）—— 只在 `.page` 内找，避开全局横幅。 */
const localError = () => {
  const node = document.querySelector(".page .banner--error");
  return node?.textContent ?? null;
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.addManualNode.mockResolvedValue(scenarioSnapshot());
});

describe("task-151 · `run()` 失败时局部文案不编原因", () => {
  it("后端**报错**（真实原因在横幅）⇒ 局部只陈述「没有生效」+ 指向横幅，不写「链接格式错」", async () => {
    mocks.addManualNode.mockRejectedValue(
      new Error("ShadowsocksR 不被 Xray 支持，请改用 ss/vless/trojan"),
    );
    await renderAndSubmit();

    await waitFor(() => expect(localError()).not.toBeNull());
    const text = localError()!;
    expect(text).toContain("没有生效");
    expect(text, "必须指向权威来源").toContain("提示条");
    // **不许**出现被推翻的那句硬编码归因（以及任何具体原因）
    expect(text).not.toContain("解析失败");
    expect(text).not.toContain("链接格式");
    expect(text).not.toContain("ShadowsocksR");
  });

  it("后端返回**坏形状**（task-146 的守卫拦下）⇒ 同一句中性话，仍然不编原因", async () => {
    mocks.addManualNode.mockResolvedValue({});
    await renderAndSubmit();

    await waitFor(() => expect(localError()).not.toBeNull());
    const text = localError()!;
    expect(text).toContain("没有生效");
    expect(text).toContain("提示条");
    expect(text).not.toContain("解析失败");
    expect(text).not.toContain("链接格式");
  });

  it("两种失败情形的**局部文案相同**（因为调用方无法区分原因，所以不该给不同结论）", async () => {
    mocks.addManualNode.mockRejectedValue(new Error("后端说：令牌过期"));
    await renderAndSubmit();
    await waitFor(() => expect(localError()).not.toBeNull());
    const byError = localError();

    document.body.innerHTML = "";
    mocks.addManualNode.mockResolvedValue({ runtime: { running: true } });
    await renderAndSubmit();
    await waitFor(() => expect(localError()).not.toBeNull());
    const byShape = localError();

    expect(byError).toBe(byShape);
  });

  it("反例：添加**成功** ⇒ 不出现任何局部错误文案，且输入被清空", async () => {
    mocks.addManualNode.mockResolvedValue({ ...scenarioSnapshot(), app_version: "9.9.9" });
    await renderAndSubmit();

    // 成功 ⇒ 收起手动添加区（输入被清空 + `setAdding(false)`），且没有局部错误文案
    await waitFor(() => expect(screen.queryByPlaceholderText(/三种都行/)).toBeNull());
    expect(localError()).toBeNull();
  });
});
