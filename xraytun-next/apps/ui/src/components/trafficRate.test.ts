// deriveRate / sampleFrom 的自检：重点是「什么时候必须返回 null」。
//
// 速率是本项目最容易出现「看起来很合理但其实是编的」的地方，
// 所以每条不能算的情形都单独一条断言。

import { describe, expect, it } from 'vitest';
import type { StatsView } from '../transport/contract';
import { deriveRate, INITIAL_RATE, sampleFrom } from './trafficRate';

describe('deriveRate', () => {
  it('初始状态：没有基准，也没有速率', () => {
    expect(INITIAL_RATE).toEqual({ prev: null, rate: null });
  });

  it('首个样本：只建立基准，rate 为 null', () => {
    const next = deriveRate(INITIAL_RATE, { bytes: 1000, atMs: 100 });
    expect(next.prev).toEqual({ bytes: 1000, atMs: 100 });
    expect(next.rate).toBe(null);
  });

  it('(100ms, 1000B) → (200ms, 3000B) 得 rate = 20000 B/s', () => {
    const first = deriveRate(INITIAL_RATE, { bytes: 1000, atMs: 100 });
    const second = deriveRate(first, { bytes: 3000, atMs: 200 });
    expect(second.rate).toBe(20000);
    expect(second.prev).toEqual({ bytes: 3000, atMs: 200 });
  });

  it('同一 atMs：算不出速率，且基准不动', () => {
    const first = deriveRate(INITIAL_RATE, { bytes: 1000, atMs: 100 });
    const second = deriveRate(first, { bytes: 9000, atMs: 100 });
    expect(second.rate).toBe(null);
    expect(second.prev).toBe(first.prev);
  });

  it('时刻倒退：算不出速率，且基准不动', () => {
    const first = deriveRate(INITIAL_RATE, { bytes: 1000, atMs: 200 });
    const second = deriveRate(first, { bytes: 9000, atMs: 100 });
    expect(second.rate).toBe(null);
    expect(second.prev).toBe(first.prev);
  });

  it('计数回退：rate 为 null，且基准更新为新样本', () => {
    const first = deriveRate(INITIAL_RATE, { bytes: 5000, atMs: 200 });
    const second = deriveRate(first, { bytes: 400, atMs: 300 });
    expect(second.rate).toBe(null);
    expect(second.prev).toEqual({ bytes: 400, atMs: 300 });
  });

  it('计数相等：速率为 0（这是合法的 0，不是未知）', () => {
    const first = deriveRate(INITIAL_RATE, { bytes: 5000, atMs: 200 });
    const second = deriveRate(first, { bytes: 5000, atMs: 400 });
    expect(second.rate).toBe(0);
    expect(second.prev).toEqual({ bytes: 5000, atMs: 400 });
  });
});

describe('sampleFrom', () => {
  const stats: StatsView = { uplink_bytes: 111, downlink_bytes: 222, sampled_at_ms: 1700 };

  it('stats 缺失 → null', () => {
    expect(sampleFrom(null, 'uplink')).toBe(null);
    expect(sampleFrom(undefined, 'downlink')).toBe(null);
  });

  it('采样时刻非法 → null（NaN / 0 / 负数 / 无穷）', () => {
    expect(sampleFrom({ ...stats, sampled_at_ms: Number.NaN }, 'uplink')).toBe(null);
    expect(sampleFrom({ ...stats, sampled_at_ms: 0 }, 'uplink')).toBe(null);
    expect(sampleFrom({ ...stats, sampled_at_ms: -5 }, 'uplink')).toBe(null);
    expect(sampleFrom({ ...stats, sampled_at_ms: Number.POSITIVE_INFINITY }, 'uplink')).toBe(null);
  });

  it('合法 stats → 按方向取对应字节数', () => {
    expect(sampleFrom(stats, 'uplink')).toEqual({ bytes: 111, atMs: 1700 });
    expect(sampleFrom(stats, 'downlink')).toEqual({ bytes: 222, atMs: 1700 });
  });
});
