/**
 * 日志的「跟随滚动」行为。
 *
 * # 为什么抽成 hook
 *
 * 原来的实现是「只要开关开着，每次日志变化就把视图滚到底」。后果：
 * **用户往上翻想看历史时，下一条日志立刻把他拽回底部** —— 表现就是
 * 「根本滚不动」。实测（CDP 量 scrollTop）：跟随开时手动滚到 50，
 * 新日志到达后立刻弹回 2065。
 *
 * 现在的判据改成**用滚动位置本身表达意图**：
 *
 * * 用户往上滚（离开底部）→ 自动暂停跟随，开关同步变成关闭；
 * * 用户滚回底部 → 自动恢复跟随；
 * * 开关仍然是那个开关，不再和用户的滚动打架。
 *
 * 抽成 hook 也是为了能测：这段逻辑跨越「状态 + DOM 滚动位置 + 事件」，
 * 塞在组件里只能用浏览器手工验，而它恰恰是容易出回归的地方。
 */

import { useCallback, useEffect, useRef, useState } from "react";

/** 距底部多少像素以内算「在底部」。用 8px 而不是 0：亚像素与圆整会差一两像素。 */
export const BOTTOM_SLACK_PX = 8;

/** 当前滚动位置是否算「贴着底部」。 */
export function atBottom(el: {
  scrollHeight: number;
  clientHeight: number;
  scrollTop: number;
}): boolean {
  return el.scrollHeight - el.clientHeight - el.scrollTop <= BOTTOM_SLACK_PX;
}

/** 把元素滚到底。jsdom 里没有 `scrollIntoView` 的实现，所以用 scrollTop。 */
function scrollToBottom(el: HTMLElement): void {
  el.scrollTop = el.scrollHeight;
}

export function useFollowScroll(contentLength: number) {
  const boxRef = useRef<HTMLDivElement | null>(null);
  const bottomRef = useRef<HTMLDivElement | null>(null);
  const [follow, setFollow] = useState(true);

  // 用户往上滚就暂停跟随；滚回底部就恢复。
  //
  // 程序性滚动也会触发 scroll 事件，但那时我们本来就在底部，`atBottom` 为真，
  // 于是只会把 follow 再设成 true（幂等），不会误判成「用户滚开了」。
  const onScroll = useCallback(() => {
    const el = boxRef.current;
    if (!el) return;
    const near = atBottom(el);
    setFollow((prev) => (prev === near ? prev : near));
  }, []);

  // 有新内容：只有仍处于跟随状态时才滚到底。
  useEffect(() => {
    if (!follow) return;
    const el = boxRef.current;
    if (el) {
      scrollToBottom(el);
      return;
    }
    // 兜底：容器还没量到（首帧）时用元素自身滚入视野。
    bottomRef.current?.scrollIntoView?.({ block: "end" });
  }, [contentLength, follow]);

  return { boxRef, bottomRef, follow, setFollow, onScroll };
}
