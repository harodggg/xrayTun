// 速率派生：只算数，不做格式化、不做平滑、不在缺样本时给一个好看的数字。
//
// 「rate 为 null」是一个有意义的返回值，意思是「现在不许显示速率」：
//  - 只有第一个样本时，没有时间差，算不出速率；
//  - 时刻没有前进（重复采样）时，分母为零，算不出速率；
//  - 计数变小说明采样序列断了（daemon 重启 / 计数器重置），此时旧基准作废，
//    以新样本重新起算，同样不显示速率。
//
// 这里刻意不做任何「上次的值先用着」那种补位：显示一个编造出来的速率，
// 比显示「未知」坏得多。

import type { StatsView } from '../transport/contract';

export interface RateSample {
  bytes: number;
  atMs: number;
}

export interface RateState {
  prev: RateSample | null;
  rate: number | null;
}

export const INITIAL_RATE: RateState = { prev: null, rate: null };

/** 吃一个新样本，返回新状态。rate 为 null =「现在不许显示速率」。 */
export function deriveRate(state: RateState, current: RateSample): RateState {
  if (state.prev === null) {
    return { prev: current, rate: null };
  }
  if (current.atMs <= state.prev.atMs) {
    return { ...state, rate: null };
  }
  if (current.bytes < state.prev.bytes) {
    return { prev: current, rate: null };
  }
  const deltaBytes = current.bytes - state.prev.bytes;
  const deltaMs = current.atMs - state.prev.atMs;
  return { prev: current, rate: Math.round((deltaBytes * 1000) / deltaMs) };
}

/** 从 StatsView 取上行/下行样本；不可用时返回 null。 */
export function sampleFrom(
  stats: StatsView | null | undefined,
  which: 'uplink' | 'downlink',
): RateSample | null {
  if (stats == null) return null;
  if (!Number.isFinite(stats.sampled_at_ms) || stats.sampled_at_ms <= 0) return null;
  return {
    bytes: which === 'uplink' ? stats.uplink_bytes : stats.downlink_bytes,
    atMs: stats.sampled_at_ms,
  };
}
