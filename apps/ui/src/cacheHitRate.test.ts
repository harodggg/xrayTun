/**
 * 0.9.1 · 大模型判决缓存的**命中率**必须能被读出来（本地计数，无遥测）。
 *
 * 这里只测纯函数：文案口径与「缺字段不编造」的边界。
 * 真实命中率要靠 `crates/xt-intent` 的计数 + 用户在真机上看这一行。
 */

import { describe, expect, it } from "vitest";

import { cacheHitRateText } from "./pages/Intent";

describe("0.9.1 · 判决缓存命中率文案", () => {
  it("口径：cache_inherited 是 cache_hits 的**子集**，命中率 = hits/(hits+misses)", () => {
    // 8 次命中里有 2 次是继承来的 ⇒ 命中率仍是 8/18，不是 10/18
    const text = cacheHitRateText({ cache_hits: 8, cache_inherited: 2, cache_misses: 10 })!;
    expect(text).toContain("44%");
    expect(text).toContain("18 次里 8 次没问大模型");
    expect(text).toContain("其中继承父域 2");
    expect(text).toContain("未命中 10");
    // 关键反例：把继承再加一遍会得到 56%，那是错的
    expect(text).not.toContain("56%");
  });

  it("继承为 0 时不显示继承那段", () => {
    const text = cacheHitRateText({ cache_hits: 1, cache_inherited: 0, cache_misses: 1 })!;
    expect(text).toContain("50%");
    expect(text).not.toContain("继承");
  });

  it("继承数异常大于命中数时按命中数夹住（不出现 >100% 的荒谬值）", () => {
    const text = cacheHitRateText({ cache_hits: 2, cache_inherited: 99, cache_misses: 0 })!;
    expect(text).toContain("100%");
    expect(text).toContain("继承父域 2");
  });

  it("**缺 cache_misses**（旧后端）⇒ 返回 null：没有分母就不编造百分比", () => {
    expect(cacheHitRateText({ cache_hits: 42 })).toBeNull();
  });

  it("一次查询都没有 ⇒ 明说「还没有可统计的判定」，**不许**写成 0%", () => {
    const text = cacheHitRateText({ cache_hits: 0, cache_misses: 0 })!;
    expect(text).toContain("还没有可统计的判定");
    expect(text).not.toContain("0%");
  });

  it("缺 cache_inherited（旧后端只有 hits+misses）⇒ 按 0 处理但照常给百分比", () => {
    const text = cacheHitRateText({ cache_hits: 3, cache_misses: 1 })!;
    expect(text).toContain("75%");
  });

  it("网关失败冷却跳过的次数要如实说（它是省钱，不是命中）", () => {
    const text = cacheHitRateText({
      cache_hits: 1,
      cache_misses: 1,
      cooldown_skipped: 4,
    })!;
    expect(text).toContain("50%");
    expect(text).toContain("4 次因网关失败冷却跳过提问");
  });

  it("冷却为 0 时不提它（不制造噪音）", () => {
    const text = cacheHitRateText({ cache_hits: 1, cache_misses: 1, cooldown_skipped: 0 })!;
    expect(text).not.toContain("冷却");
  });
});
