//! 黑屏兜底（`fatalError.tsx`）的判据。
//!
//! 为什么值得单独测：这套代码只在"React 已经不可用"的时刻运行 —— 正是最不该
//! 靠人手点一遍的那种路径。判据是**可见 + 可复制**：用户必须能拿到原文。

import { beforeEach, describe, expect, it, vi } from "vitest";
import { formatError, showFatal } from "./fatalError";

function fakeClipboard() {
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
  return writeText;
}

describe("界面黑屏兜底", () => {
  beforeEach(() => {
    document.body.innerHTML = '<div id="root"></div>';
  });

  it("formatError：Error / 字符串 / 普通对象都能变成可显示文本", () => {
    expect(formatError(new Error("boom")).message).toBe("boom");
    expect(formatError("裸字符串").message).toBe("裸字符串");
    expect(formatError({ a: 1 }).message).toContain('"a"');
    // 循环引用不许再抛（否则兜底自己崩，又变黑屏）。
    const cyc: Record<string, unknown> = {};
    cyc.self = cyc;
    expect(() => formatError(cyc)).not.toThrow();
  });

  it("#root 是空的 ⇒ 画整页面板，并把原文写进剪贴板", () => {
    const writeText = fakeClipboard();
    showFatal(new Error("渲染炸了"));
    const panel = document.querySelector('[data-testid="fatal-panel"]');
    expect(panel).not.toBeNull();
    expect(panel!.textContent).toContain("渲染炸了");
    expect(writeText).toHaveBeenCalledOnce();
    expect(String(writeText.mock.calls[0]?.[0])).toContain("渲染炸了");
  });

  it("#root 已有界面 ⇒ 只加横幅，不毁掉可用界面", () => {
    const root = document.getElementById("root")!;
    root.innerHTML = '<div data-testid="alive">还在</div>';
    showFatal(new Error("小错"));
    expect(document.querySelector('[data-testid="alive"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="fatal-banner"]')).not.toBeNull();
    // 幂等：第二个错误不再叠第二条横幅。
    showFatal(new Error("又来一个"));
    expect(document.querySelectorAll('[data-testid="fatal-banner"]').length).toBe(1);
  });
});
