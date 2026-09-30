// 测试进程的匹配器注册（只在 vitest 的 setupFiles 里被加载，不进任何构建产物）。
//
// 为什么放在 src/store 下：本层的写入范围是 apps/ui/src/{transport,store}（00-CONTRACT-FREEZE.md §9），
// 而 setupFiles 必须指向一个真实文件；文件名刻意不叫 *.test.ts，避免被当成测试用例。
import '@testing-library/jest-dom/vitest';
