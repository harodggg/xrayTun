/**
 * 日志「关闭跟随后还是一直跳动」的 DOM 层回归测试（task-55）。
 *
 * # ⚠️ 用户报了**两次**，但这两次不是同一个 bug —— 先把路径分清
 *
 * 1. **`36335b4` 修的是**：`useFollowScroll` 的 `onScroll` 把**显式关闭**的跟随悄悄
 *    设回 `true`（手动关闭被位置翻案）。那是真 bug，但**只影响「自动滚动」这一条路径**，
 *    由 `useFollowScroll.test.tsx` 钉住。
 * 2. **本文件钉的是主因**：`Logs.tsx` 的 key 是 `` `${line.ts_unix}-${i}` `` ——
 *    **下标进了 key**，而缓冲满员（`MAX_UI_LOGS=1500`）后每来一行就从**前面**裁掉一行
 *    → 所有元素的下标整体前移 → **所有 key 全部改变 → React 卸载并重建全部 1500 行**。
 *    它与「跟随」开关**完全无关**，所以**关掉跟随也照样跳**；核心 stdout 持续转发，
 *    于是每行都发生。这正是用户第二次报「还会一直跳动」的原因。
 *
 * # 判据为什么是「DOM 节点身份」
 *
 * 跳动的量在 jsdom 里量不到（没有布局），但**重建**可以精确量：同一行若被 React 复用，
 * 它的 DOM 节点必须是**同一个对象**。所以用 `toBe`（同一性），而不是比较文本或属性 ——
 * 文本相同也可能是被重建的新节点，而「被重建」正是要抓的那个东西。
 *
 * 另外两条：跟随关闭时 `scrollTop` 不得被程序性改动；跟随**开着**时必须仍然贴底
 * （不许把 `36335b4` 的既有行为修坏）。
 */
import { act, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  clearLogs: vi.fn(),
  diagnostics: vi.fn(),
  handlers: null as null | { onLog?: (p: { line: string; level: string }) => void },
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    clearLogs: mocks.clearLogs,
    diagnostics: mocks.diagnostics,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: (h: typeof mocks.handlers) => {
    mocks.handlers = h;
    return () => {};
  },
}));

import Logs from "./pages/Logs";
import { scenarioSnapshot } from "./previewSnapshot";
import { MAX_UI_LOGS, StoreProvider } from "./store";

/** 历史日志：故意让**同一秒内多行**（真实日志就是这样，所以 `ts_unix` 不能当身份）。 */
function seedHistory(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    ts_unix: 1_700_000_000 + Math.floor(i / 10),
    source: "core",
    level: i % 50 === 0 ? "warn" : "info",
    message: `seed line ${i}`,
  }));
}

async function renderLogs(seedCount = MAX_UI_LOGS) {
  // 同上（task-128 复盘）：快照替身必须**完整**，不能只给 `runtime`。
  // 部分形状的替身会让「页面多读一个字段」变成 **unhandled error**，
  // 而 unhandled error 在 vitest 里只体现在 `Errors N` 这一行 —— 通过数看不出来。
  {
    const base = scenarioSnapshot();
    mocks.snapshot.mockResolvedValue({
      ...base,
      runtime: { ...base.runtime, running: true },
    } as never);
  }
  mocks.tailLogs.mockResolvedValue(seedHistory(seedCount));
  const r = render(
    <StoreProvider>
      <Logs />
    </StoreProvider>,
  );
  await waitFor(() => {
    expect(r.container.querySelectorAll(".log-line").length).toBe(seedCount);
  });
  return r;
}

const rows = (c: HTMLElement): HTMLElement[] => [
  ...c.querySelectorAll<HTMLElement>(".log-line"),
];
const seqs = (c: HTMLElement): string[] => rows(c).map((el) => el.dataset.logSeq ?? "?");

/** 追加一行「核心实时日志」—— 走真实的 `onLog` → store → React 这条路径。 */
function appendLine(text: string) {
  act(() => {
    mocks.handlers?.onLog?.({ line: text, level: "info" });
  });
}

/** 关掉「跟随」（走真实的复选框）。 */
function turnFollowOff(container: HTMLElement) {
  const box = container.querySelector<HTMLInputElement>(".logs-bar__follow input");
  if (!box) throw new Error("找不到跟随复选框");
  act(() => {
    box.click();
  });
  expect(box.checked, "跟随应当已被用户关闭").toBe(false);
}

const theBox = (c: HTMLElement) => {
  const el = c.querySelector<HTMLElement>(".logs");
  if (!el) throw new Error("找不到滚动容器 .logs");
  return el;
};

// ---------------------------------------------------------------------------
// 假布局：jsdom 不做布局，`getBoundingClientRect()` 恒为 0
// ---------------------------------------------------------------------------

const ROW_H = 18;
const realGetRect = Element.prototype.getBoundingClientRect;

/**
 * 一个**最小但自洽**的布局模型：`.log-line` 的 top = 它在容器里的下标 × 行高 − `scrollTop`。
 * 这样「裁掉前面一行 → 下面所有行的 rect.top 上移一行」就真的可观测，补偿量才量得出来。
 */
function installFakeLayout(): void {
  Element.prototype.getBoundingClientRect = function (this: Element): DOMRect {
    const el = this as HTMLElement;
    if (!el.classList.contains("log-line")) {
      return { top: 0, bottom: 0, left: 0, right: 0, width: 0, height: 0, x: 0, y: 0, toJSON: () => ({}) } as DOMRect;
    }
    const box = el.closest<HTMLElement>(".logs");
    const parent = el.parentElement;
    const idx = parent ? [...parent.children].indexOf(el) : 0;
    const top = idx * ROW_H - (box?.scrollTop ?? 0);
    return {
      top,
      bottom: top + ROW_H,
      left: 0,
      right: 100,
      width: 100,
      height: ROW_H,
      x: 0,
      y: top,
      toJSON: () => ({}),
    } as DOMRect;
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.handlers = null;
  mocks.clearLogs.mockResolvedValue(undefined);
  mocks.diagnostics.mockResolvedValue("diag");
  // jsdom 不实现 scrollIntoView（useFollowScroll 的首帧兜底会用到）。
  Element.prototype.scrollIntoView = vi.fn();
});

afterEach(() => {
  Element.prototype.getBoundingClientRect = realGetRect;
});

describe("日志列表的 DOM 稳定性（task-55 主因）", () => {
  it("缓冲已满时追加一行：其余 1499 行必须是**同一批 DOM 节点对象**（不是被重建的新节点）", async () => {
    const { container } = await renderLogs(MAX_UI_LOGS);

    const before = rows(container);
    expect(before.length).toBe(MAX_UI_LOGS);
    const beforeSeqs = seqs(container);
    const firstSeq = beforeSeqs[0]!;

    appendLine("实时新行");

    const after = rows(container);
    expect(after.length, "缓冲满员后应当仍是 1500 行（丢掉最旧一行）").toBe(MAX_UI_LOGS);

    // 新行在，且拿到了**新的**身份
    const afterBySeq = new Map(after.map((el) => [el.dataset.logSeq!, el]));
    expect(afterBySeq.size, "key 必须唯一").toBe(MAX_UI_LOGS);
    expect(
      afterBySeq.has(firstSeq),
      `最旧的一行（seq=${firstSeq}）应当被裁掉`,
    ).toBe(false);

    // ★ 核心：其余每一行都必须是**同一个对象**
    let checked = 0;
    for (let i = 0; i < before.length; i++) {
      const seq = beforeSeqs[i]!;
      if (seq === firstSeq) continue; // 这一行被裁掉，本来就不该在
      expect(afterBySeq.get(seq), `seq=${seq} 的行被重建了（不是同一个 DOM 节点）`).toBe(
        before[i]!,
      );
      checked += 1;
    }
    expect(checked, "应当逐行核对 1499 个幸存节点").toBe(MAX_UI_LOGS - 1);
  });

  it("身份必须来自 seq：同一秒内的多行 key 也互不相同（ts_unix 只到秒，不能当身份）", async () => {
    const { container } = await renderLogs(3);
    appendLine("a");
    appendLine("b");

    const all = seqs(container);
    expect(new Set(all).size, `key 重复了：${all.join(",")}`).toBe(all.length);
  });

  it("跟随关闭 + 缓冲已满 + 追加 100 行：scrollTop 不得被程序性改动（变化量 0）", async () => {
    const { container } = await renderLogs(MAX_UI_LOGS);
    const box = theBox(container);
    turnFollowOff(container);

    // jsdom 没有布局 → 行高为 0 → 补偿量也是 0。这一条量的是「有没有人擅自滚」。
    box.scrollTop = 500;
    for (let i = 0; i < 100; i++) appendLine(`实时 ${i}`);

    expect(box.scrollTop, "跟随关闭时不允许改动 scrollTop").toBe(500);
  });

  it("跟随关闭 + 假布局：裁掉旧行时**阅读位置不动**（scrollTop 按被裁高度补偿）", async () => {
    installFakeLayout();
    const { container } = await renderLogs(MAX_UI_LOGS);
    const box = theBox(container);
    turnFollowOff(container);

    box.scrollTop = 5000;
    // 真实浏览器在滚动后**会发 `scroll` 事件**（jsdom 不会），补偿的基准靠它刷新。
    box.dispatchEvent(new Event("scroll"));
    // 锚点 = 最后一行（裁剪只从前面发生，所以它活得最久）
    const anchor = rows(container)[MAX_UI_LOGS - 1]!;
    const viewportOffsetBefore = anchor.getBoundingClientRect().top; // 相对视口

    for (let i = 0; i < 100; i++) appendLine(`实时 ${i}`);

    // 100 行被裁掉 ⇒ scrollTop 必须减少 100 × 行高，视觉位置才不动
    expect(box.scrollTop, `scrollTop 应为 5000 − 100×${ROW_H}`).toBe(5000 - 100 * ROW_H);
    // 锚点相对视口的位置不变 —— 这才是「阅读位置不动」的直接判据
    expect(anchor.getBoundingClientRect().top).toBe(viewportOffsetBefore);
    expect(anchor.isConnected, "锚点行本身不该被裁掉").toBe(true);
  });

  it("反例：跟随**开着**时仍然贴底（`36335b4` 的行为不许破坏）", async () => {
    const { container } = await renderLogs(MAX_UI_LOGS);
    const box = theBox(container);
    Object.defineProperty(box, "scrollHeight", { value: 20000, configurable: true });

    // 默认跟随是开的
    const checkbox = container.querySelector<HTMLInputElement>(".logs-bar__follow input")!;
    expect(checkbox.checked).toBe(true);

    box.scrollTop = 100;
    appendLine("实时新行");

    expect(box.scrollTop, "跟随开着时必须滚到底").toBe(box.scrollHeight);
  });
});
