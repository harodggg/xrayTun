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
  const [follow, setFollowState] = useState(true);

  /**
   * 用户是否**显式关掉**过跟随。
   *
   * # 为什么必须有这个标记
   *
   * 位置驱动的自动暂停/恢复（见下面的 `onScroll`）与复选框共用同一个 state 时，
   * 两者会互相翻案 —— 实测到的用户问题是：
   *
   *   1. 用户在底部取消勾选「跟随」（意图：别再自动滚了）；
   *   2. 他随手一滚（`atBottom` 仍为真）→ `onScroll` 把 follow 设回 `true`；
   *   3. 下一条日志又把他拽回底部。
   *
   * 也就是**「关了跟随，日志还在动」** —— 开关形同虚设。
   *
   * 语义上这两件事必须分开：
   * * **位置**只说明「用户现在看的是哪里」，可以据此*自动暂停*（往上翻历史时别打断他）；
   * * **复选框**是用户的*明确指令*，一旦说关就必须是关，位置不许翻案。
   *
   * 所以：显式关闭后锁住（位置驱动完全停用），直到用户再次显式打开。
   */
  const manualOffRef = useRef(false);

  /** 复选框走这里：它是用户的明确指令，所以顺带记录意图。 */
  const setFollow = useCallback((next: boolean) => {
    manualOffRef.current = !next;
    setFollowState(next);
  }, []);

  // 用户往上滚就暂停跟随；滚回底部就恢复。
  //
  // 程序性滚动也会触发 scroll 事件，但那时我们本来就在底部，`atBottom` 为真，
  // 于是只会把 follow 再设成 true（幂等），不会误判成「用户滚开了」。
  const onScroll = useCallback(() => {
    const el = boxRef.current;
    if (!el) return;
    // 显式关过：位置不许把它翻回来（否则开关等于没关）。
    if (manualOffRef.current) return;
    const near = atBottom(el);
    setFollowState((prev) => (prev === near ? prev : near));
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
