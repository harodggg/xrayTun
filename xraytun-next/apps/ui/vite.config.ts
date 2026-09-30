import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// 应用外壳在 index.html → /src/main.tsx（ui 的所有权）。
// 为什么 target 是 es2022：产物只跑在 WebView 里（macOS 上是 WKWebView），
// 转译到更老的语法只会让 sourcemap 更难读，不会换来兼容性。
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: true,
    rollupOptions: {
      // node:net 只由 Node 提供（AF_UNIX 在 WebView/浏览器里不存在）。
      // unixSocket.ts 用动态 import 加载它，只有真正走 Node 通道时才会执行到；
      // 这里声明为 external，浏览器主包里就不会出现 node 内置模块。
      external: ['node:net'],
    },
  },
});
