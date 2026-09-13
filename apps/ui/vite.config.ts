import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri 会把 dist/ 作为 frontendDist 打包进去，所以 outDir 必须是 dist。
export default defineConfig({
  plugins: [react()],
  // Tauri 自己会清屏，Vite 再清一次会让 Rust 侧的编译错误被刷掉。
  clearScreen: false,
  server: {
    port: 5173,
    // 端口被占用时直接失败，而不是悄悄换一个 —— 否则 Tauri 的 devUrl 会指向空地址。
    strictPort: true,
  },
  build: {
    // 目标定为 Safari 15：macOS 13 自带的就是这个版本，是我们声明的最低系统版本。
    // 用更激进的 target 会产出 WKWebView 认不出的语法，表现为白屏。
    target: "safari15",
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
  },
});
