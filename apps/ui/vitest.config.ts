import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// 测试环境用 jsdom：这些测试要验的是「DOM 滚动位置 + React 状态」的交互，
// 纯 node 环境里没有滚动容器可量。
export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    // 必须为 true：`@testing-library/react` 靠全局的 afterEach 判断是否
    // 在每个测试后自动 cleanup。关掉它会导致 DOM 在测试之间累积，
    // 表现为 getByTestId 报「找到多个元素」。
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
});
