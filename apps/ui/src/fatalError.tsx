/**
 * 界面「黑屏」的兜底：任何未捕获的错误都必须**显示出来**。
 *
 * # 为什么必须有（真实事故）
 *
 * React 在未捕获的渲染异常上会卸载整棵树；而 WKWebView 里没有控制台可看 ——
 * 用户看到的是一片黑，我们连报错文本都拿不到。0.8.40 本机包就是这样：
 * 「先是出来了，之后又黑了」，后端/进程/WebView 全都正常，但没有任何可诊断信息。
 *
 * # 约定
 *
 * * 面板只写 `textContent`，**绝不 innerHTML**（错误文本可能含 HTML）；
 * * 同时把错误写进**剪贴板**：用户粘一下就能把原文给我们；
 * * `#root` 已有内容时**不覆盖**（一个无害错误不该毁掉可用界面），只加顶部横幅；
 * * 这里刻意不依赖 React 状态：React 自己崩掉时，兜底必须还能画出来。
 */

// 注：本文件用自动 JSX runtime，**不需要** `import React`（加了反而被 tsc 判未使用）。

export interface FatalInfo {
  message: string;
  detail?: string;
}

const CLIPBOARD_HEADER = "XrayTun UI error";

export function copyToClipboard(info: FatalInfo): void {
  const text = `${CLIPBOARD_HEADER}\n${info.message}\n${info.detail ?? ""}`;
  try {
    void navigator.clipboard?.writeText(text);
  } catch {
    // 剪贴板不可用（权限/非安全上下文）时静默：显示比复制重要。
  }
}

/** 把任意抛出物规范化成可显示的两段文本。 */
export function formatError(e: unknown): FatalInfo {
  if (e instanceof Error) {
    return { message: e.message || String(e), detail: e.stack };
  }
  if (typeof e === "string") return { message: e };
  try {
    return { message: JSON.stringify(e) };
  } catch {
    return { message: String(e) };
  }
}

export function fatalText(info: FatalInfo): string {
  return `界面出错，已复制到剪贴板（请把这段发给我）\n\n${info.message}${
    info.detail ? `\n\n${info.detail}` : ""
  }`;
}

/** React 里用的面板（ErrorBoundary 渲染它）。 */
export function FatalPanel({ info }: { info: FatalInfo }) {
  return (
    <pre
      data-testid="fatal-panel"
      style={{
        whiteSpace: "pre-wrap",
        margin: 0,
        padding: "16px 18px",
        minHeight: "100vh",
        background: "#16161a",
        color: "#ffd9d9",
        font: "12px/1.6 ui-monospace, SFMono-Regular, Menlo, monospace",
      }}
    >
      {fatalText(info)}
    </pre>
  );
}

/**
 * 全局兜底：`#root` 为空 ⇒ 画整页面板；已有内容 ⇒ 只加一条不遮挡操作的横幅。
 *
 * 用**纯 DOM** 而不是 React：走到这里时 React 可能已经不可用了。
 */
export function showFatal(e: unknown): void {
  const info = formatError(e);
  copyToClipboard(info);
  const root = document.getElementById("root");
  if (!root) return;

  if (root.childElementCount === 0) {
    root.replaceChildren();
    const pre = document.createElement("pre");
    pre.setAttribute("data-testid", "fatal-panel");
    pre.style.cssText =
      "white-space:pre-wrap;margin:0;padding:16px 18px;min-height:100vh;" +
      "background:#16161a;color:#ffd9d9;" +
      "font:12px/1.6 ui-monospace,SFMono-Regular,Menlo,monospace";
    pre.textContent = fatalText(info);
    root.appendChild(pre);
    return;
  }

  if (document.getElementById("fatal-banner")) return;
  const bar = document.createElement("div");
  bar.id = "fatal-banner";
  bar.setAttribute("data-testid", "fatal-banner");
  bar.style.cssText =
    "position:fixed;left:0;right:0;top:0;z-index:2147483647;padding:8px 12px;" +
    "background:#7f1d1d;color:#fff;white-space:pre-wrap;" +
    "font:12px/1.5 ui-monospace,SFMono-Regular,Menlo,monospace";
  bar.textContent = `界面出错（已复制到剪贴板）：${info.message}`;
  document.body.appendChild(bar);
}

/** 装上 `error` / `unhandledrejection` 两个全局钩子。幂等。 */
export function installFatalHandlers(): void {
  if ((window as unknown as Record<string, unknown>).__xraytunFatalInstalled) return;
  (window as unknown as Record<string, unknown>).__xraytunFatalInstalled = true;

  window.addEventListener("error", (event) => {
    // 资源加载失败（img/script）没有 error 对象：不当成崩溃，只记横幅文案来源。
    showFatal(event.error ?? event.message);
  });
  window.addEventListener("unhandledrejection", (event) => {
    showFatal(event.reason);
  });
}
