import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

/**
 * 敏感性实验用配置（verifier-0.9）。
 *
 * 与产品 `vitest.config.ts` 同构，**只多一条 alias**：把某个产品模块的绝对路径
 * 重定向到 `__verify09__/mutants/` 下的 mutant 副本 —— 这样能在**不改产品文件**
 * 的前提下，让产品自己的测试跑在"故意破坏一处判据"的版本上。
 *
 *   VERIFY09_FIND='^\\.\\./pages/Logs$' \
 *   VERIFY09_MUTANT=/abs/apps/ui/src/__verify09__/mutants/d2_logs.tsx \
 *   npx vitest run src/__verify09__/p0_probe.test.tsx --config src/__verify09__/mutant.vitest.config.ts
 */
const find = process.env.VERIFY09_FIND;      // 对 **import specifier** 生效的正则
const mutant = process.env.VERIFY09_MUTANT;

export default defineConfig({
  plugins: [react()],
  resolve:
    find && mutant
      ? { alias: [{ find: new RegExp(find), replacement: mutant }] }
      : {},
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    globals: true,
    setupFiles: ["./src/test-setup.ts"],
  },
});
