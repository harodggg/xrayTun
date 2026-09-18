/**
 * 前端测试的公共准备。
 *
 * jsdom 不实现 `scrollIntoView`（没有布局引擎），而日志页的兜底分支会用到它 ——
 * 缺了它测试会因为「不是函数」直接失败，掩盖掉真正要验的行为。
 */
import { vi } from "vitest";

if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = vi.fn();
}
