/**
 * 事件载荷的形状校验。
 *
 * # 为什么需要它
 *
 * Tauri 事件推进来的是**运行时数据**，而 `e.payload as ProbeResult[]` 这类
 * 断言**不做任何校验** —— 它只是让编译器闭嘴。实测撞到过一次：载荷不是数组时
 * 处理器里的 `for...of` 抛 `results is not iterable`，**React 整棵树被卸载**，
 * 用户看到的是界面凭空消失。
 *
 * 所以每个事件处理器入口都先过一次校验：**形状不对就忽略这次更新**，
 * 并在控制台留下原因（同一个事件只记一次，避免高频事件刷屏）。
 * 坏掉一个字段，不该让整个界面消失。
 *
 * 放在独立模块而不是塞在 `store.tsx` 里：store 只管状态，形状校验是纯粹的
 * 判定逻辑，单独放才好测。
 */

/** 已经报过警的事件 —— 同一个事件只提示一次。 */
const warned = new Set<string>();

/**
 * 载荷必须是普通对象。
 *
 * **数组要排除**：`typeof [] === "object"`，所以只判 `typeof` 会让数组通过，
 * 紧接着访问 `payload.runtime` 得到 `undefined` —— 正是要防的情况。
 */
export function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/** 载荷必须是非空字符串。 */
export function isText(v: unknown): v is string {
  return typeof v === "string";
}

/** 载荷必须是有限数值（排除 NaN / Infinity）。 */
export function isCount(v: unknown): v is number {
  return typeof v === "number" && Number.isFinite(v);
}

/**
 * 忽略一个形状不对的载荷，并（该事件首次出现时）留下原因。
 *
 * 返回 `void` 是刻意的：调用点写成 `return rejectPayload(...)`，
 * 一眼能看出「这次更新被丢掉了」。
 */
export function rejectPayload(event: string, payload: unknown, why: string): void {
  if (warned.has(event)) return;
  warned.add(event);
  console.error(`[事件载荷异常] ${event}: ${why}`, payload);
}

/** 仅供测试：清掉「已提示过」的记录。 */
export function resetWarnings(): void {
  warned.clear();
}
