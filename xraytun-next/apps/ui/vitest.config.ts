import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// 为什么单独一份 vitest 配置而不是塞进 vite.config.ts：存在 vitest.config.ts 时
// vitest 只会读这一份（vite.config.ts 会被忽略），把测试配置写在这里才不会出现
// 「改了 vite.config 的 test 段却不生效」这种假配置。
export default defineConfig({
  plugins: [react()],
  test: {
    // 界面测试需要 DOM；node 传输测试在文件头用 // @vitest-environment jsdom 覆盖默认值即可。
    environment: 'jsdom',
    globals: true,
    // tests/** 是 ux 的所有权（00-CONTRACT-FREEZE.md §9），必须被覆盖到。
    include: ['src/**/*.test.{ts,tsx}', 'tests/**/*.test.{ts,tsx}'],
    // jest-dom 的匹配器只在测试进程里注册（文件说明见 test-setup.ts）。
    setupFiles: ['src/store/test-setup.ts'],
  },
});
