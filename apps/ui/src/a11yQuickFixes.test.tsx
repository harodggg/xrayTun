/**
 * task-23 可访问性快修三件（A4 / B1 / B3）。
 *
 * # 原来用户会看到什么错的
 *
 * * **A4**：`--text-faint: #64748b` 在 11px 正文上对 `--bg` / `--bg-elevated` 只有
 *   **3.87:1 / 3.54:1**，低于 WCAG AA 的 4.5:1 —— 弱视/低质量屏幕上那些提示
 *   （「要不要手动设系统代理」「证书会不会被撤掉」）基本读不清。
 * * **B1**：节点行的「选中」只有鼠标能完成（`<div onClick>`、无 `role`/`tabIndex`）：
 *   键盘用户 Tab 只能落到行内的「二维码 / 删除」，**没有任何办法切换节点**；
 *   读屏也读不出这一组是单选、选没选中。
 * * **B3**：两处「复制」失败是静默的 —— 日志页 `catch { console.log(text) }`、
 *   节点导出弹窗连 `catch` 都没有。剪贴板被拒时用户以为复制成功，贴出去是空的。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  incidentAnomalyCount: vi.fn(),
  selectNode: vi.fn(),
  deleteNode: vi.fn(),
  exportNode: vi.fn(),
  testLatency: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
      selectNode: mocks.selectNode,
      deleteNode: mocks.deleteNode,
      exportNode: mocks.exportNode,
      testLatency: mocks.testLatency,
    },
    subscribe: () => () => {},
  };
});

import Logs from "./pages/Logs";
import Nodes from "./pages/Nodes";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

// ---- 读源码 / 色彩计算 -----------------------------------------------------
// 与 `topbarStatus.test.tsx` 同一套路：本包不装 `@types/node`，所以走
// **非字面量**动态 import（TS 静态解析不到 ⇒ 返回 any）。
let readSrc: (rel: string) => string;
beforeAll(async () => {
  const fs = (await import("node:fs" as string)) as {
    readFileSync: (p: string, encoding: string) => string;
  };
  const path = (await import("node:path" as string)) as {
    resolve: (...parts: string[]) => string;
  };
  readSrc = (rel) => fs.readFileSync(path.resolve("src", rel), "utf8");
});

const tokens = (): Record<string, string> => {
  const block = readSrc("styles.css").match(/:root\s*\{([\s\S]*?)\}/)?.[1] ?? "";
  const out: Record<string, string> = {};
  for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
    const name = m[1];
    const value = m[2];
    if (name && value) out[name] = value.trim();
  }
  return out;
};

function luminance(hex: string): number {
  const parts = [1, 3, 5].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const [r, g, b] = parts.map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)) as [
    number,
    number,
    number,
  ];
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function contrast(a: string, b: string): number {
  const x = luminance(a);
  const y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  // 剪贴板被拒（权限/窗口未聚焦）—— B3 要防的正是这一态。
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: vi.fn().mockRejectedValue(new Error("denied")) },
  });
});

// ---------------------------------------------------------------------------
// A4 对比度
// ---------------------------------------------------------------------------

describe("A4：`--text-faint` 在两种背景上都要 ≥ 4.5:1（WCAG AA）", () => {
  it("11px 正文用的 --text-faint 达到 AA", () => {
    const t = tokens();
    const faint = t["--text-faint"]!;
    const bg = t["--bg"]!;
    const elevated = t["--bg-elevated"]!;
    expect(contrast(faint, bg), `--text-faint(${faint}) vs --bg 只有 ${contrast(faint, bg).toFixed(2)}:1`).toBeGreaterThanOrEqual(4.5);
    expect(
      contrast(faint, elevated),
      `--text-faint(${faint}) vs --bg-elevated 只有 ${contrast(faint, elevated).toFixed(2)}:1`,
    ).toBeGreaterThanOrEqual(4.5);
  });

  it("反证：旧值 `#64748b` 按同一公式确实不达标（这条断言本身能失败）", () => {
    const t = tokens();
    const bg = t["--bg"]!;
    const elevated = t["--bg-elevated"]!;
    expect(contrast("#64748b", bg)).toBeLessThan(4.5);
    expect(contrast("#64748b", elevated)).toBeLessThan(4.5);
    expect(t["--text-faint"]).not.toBe("#64748b");
  });
});

// ---------------------------------------------------------------------------
// B1 节点行可聚焦
// ---------------------------------------------------------------------------

describe("B1：节点列表是单选（radiogroup/radio），键盘能选中", () => {
  it("每行都是 role=radio，恰好一行 aria-checked=true", async () => {
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    const rows = await screen.findAllByRole("radio");
    expect(rows.length).toBe(scenarioSnapshot().nodes.length);
    expect(rows.filter((r) => r.getAttribute("aria-checked") === "true").length).toBe(1);
    expect(document.querySelector('[role="radiogroup"]')?.getAttribute("aria-label")).toBe("节点");
  });

  it("Enter 选中当前行（原来键盘完全没法切节点）", async () => {
    mocks.selectNode.mockResolvedValue(scenarioSnapshot());
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    const rows = await screen.findAllByRole("radio");
    fireEvent.keyDown(rows[0]!, { key: "Enter" });
    await waitFor(() => expect(mocks.selectNode).toHaveBeenCalledWith("n-hk-1"));
  });

  it("ArrowDown 把焦点移到下一行并选中它", async () => {
    mocks.selectNode.mockResolvedValue(scenarioSnapshot());
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    const rows = await screen.findAllByRole("radio");
    rows[0]!.focus();
    expect(document.activeElement).toBe(rows[0]);
    fireEvent.keyDown(rows[0]!, { key: "ArrowDown" });
    expect(document.activeElement).toBe(rows[1]);
    await waitFor(() => expect(mocks.selectNode).toHaveBeenCalledWith("n-jp-2"));
  });
});

// ---------------------------------------------------------------------------
// B3 复制失败不再静默
// ---------------------------------------------------------------------------

describe("B3：剪贴板被拒时必须说出来（并给可手动复制的兜底）", () => {
  it("日志页「复制」：出现失败说明 + 可选中 textarea（里面就是要复制的日志原文）", async () => {
    // 先给日志页一条**真实存在**的日志：`复制` 复制的是筛选后的日志原文
    // （`Logs.tsx` 的 `copyText`）。这条用例原来的 `tailLogs` 是空数组，
    // 于是「要复制的文本」是空串，`CopyButton` 只在 `state.fallback` 非空时才渲染
    // 兜底 textarea —— 断言找不到控件，但它验到的只是「没有内容可复制」这个退化态，
    // 而不是 B3 要防的「有内容、剪贴板被拒、用户以为复制成功、贴出去是空的」。
    const line = {
      seq: 1,
      ts_unix: 1_700_000_000,
      source: "core",
      level: "warn",
      message: "连接超时：握手未完成",
    };
    mocks.tailLogs.mockResolvedValue([line]);
    render(
      <StoreProvider>
        <Logs />
      </StoreProvider>,
    );
    await screen.findByText(/连接超时：握手未完成/);
    fireEvent.click(await screen.findByRole("button", { name: "复制" }));
    expect(await screen.findByText(/没有复制成功/)).toBeTruthy();
    const box = screen.getByLabelText("手动复制内容") as HTMLTextAreaElement;
    // 断言加强：兜底区里必须**就是那段日志原文**（可手动选中复制），
    // 而不只是一个存在但内容不对的空壳。
    expect(box.value).toBe(
      `[${new Date(line.ts_unix * 1000).toISOString()}] ${line.source}/${line.level} ${line.message}`,
    );
    expect(box.readOnly).toBe(true);
  });

  it("节点导出弹窗「复制链接」：同样有失败说明（原来连 catch 都没有）", async () => {
    mocks.exportNode.mockResolvedValue({
      node_id: "n-hk-1",
      node_name: "香港 01",
      uri: "vless://example",
      svg: "<svg></svg>",
      lost: [],
    });
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    const qr = await screen.findAllByRole("button", { name: "二维码" });
    fireEvent.click(qr[0]!);
    fireEvent.click(await screen.findByRole("button", { name: "复制链接" }));
    expect(await screen.findByText(/没有复制成功/)).toBeTruthy();
    expect(screen.getByLabelText("手动复制内容")).toBeTruthy();
  });
});
