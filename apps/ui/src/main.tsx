import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles.css";

/**
 * 浏览器预览：只在 **dev 构建** 且 URL 带 `?preview=1` 时挂上假后端。
 *
 * 为什么需要：界面完全依赖 Tauri 的 `invoke`，在普通浏览器里只会看到
 * "window.__TAURI_INTERNALS__ is undefined"，改一次样式却要重编整个
 * Rust 外壳。挂上桥接后布局/样式/交互都能真实渲染。
 *
 * 生产不受影响：`import.meta.env.DEV` 在打包时恒为 false，
 * 这段分支连同 preview.ts 一起被摇掉。
 */
async function bootstrap() {
  const wantPreview =
    import.meta.env.DEV && new URLSearchParams(location.search).has("preview");
  if (wantPreview) {
    const { installPreviewBridge } = await import("./preview");
    installPreviewBridge();
  }

  const root = document.getElementById("root");
  if (!root) {
    throw new Error("找不到 #root 挂载点 —— index.html 与 main.tsx 不匹配");
  }

  ReactDOM.createRoot(root).render(
    <React.StrictMode>
      <App />
    </React.StrictMode>,
  );
}

void bootstrap();
