/**
 * 0.9.1 · 大模型判决缓存的**命中率**必须能被读出来（本地计数，无遥测）。
 *
 * 这里只测纯函数：文案口径与「缺字段不编造」的边界。
 * 真实命中率要靠 `crates/xt-intent` 的计数 + 用户在真机上看这一行。
 */

import { describe, expect, it } from "vitest";

import { cacheHitRateText } from "./pages/Intent";

describe("0.9.1 · 判决缓存命中率文案", () => {
  it("三个计数齐全 ⇒ 给出百分比、总数、以及各自明细", () => {
    const text = cacheHitRateText({ cache_hits: 8, cache_inherited: 2, cache_misses: 10 });
    expect(text).toContain("50%");
    expect(text).toContain("20 次里 10 次没问大模型");
    expect(text).toContain("精确 8");
    expect(text).toContain("继承父域 2");
    expect(text).toContain("未命中 10");
  });

  it("继承命中单独列出来（不与精确命中混在一起）", () => {
    const text = cacheHitRateText({ cache_hits: 0, cache_inherited: 4, cache_misses: 0 })!;
    expect(text).toContain("100%");
    expect(text).toContain("继承父域 4");
    // 没有精确命中就不该出现「精确 0」这种噪音
    expect(text).not.toContain("精确 0");
  });

  it("继承为 0 时不显示继承那段", () => {
    const text = cacheHitRateText({ cache_hits: 1, cache_inherited: 0, cache_misses: 1 })!;
    expect(text).not.toContain("继承");
  });

  it("**缺 cache_misses**（旧后端）⇒ 返回 null：没有分母就不编造百分比", () => {
    expect(cacheHitRateText({ cache_hits: 42 })).toBeNull();
  });

  it("一次查询都没有 ⇒ 明说「还没有可统计的判定」，**不许**写成 0%", () => {
    const text = cacheHitRateText({ cache_hits: 0, cache_inherited: 0, cache_misses: 0 })!;
    expect(text).toContain("还没有可统计的判定");
    expect(text).not.toContain("0%");
  });

  it("缺 cache_inherited（旧后端只有 hits+misses）⇒ 按 0 处理但照常给百分比", () => {
    const text = cacheHitRateText({ cache_hits: 3, cache_misses: 1 })!;
    expect(text).toContain("75%");
  });
});
