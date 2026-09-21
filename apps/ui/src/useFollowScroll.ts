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

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type RefObject } from "react";

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

/**
 * @param contentRevision **内容版本**：渲染内容每变一次就必须变一个值
 *   （跟随开启时靠它决定「要不要再滚到底」）。
 *
 *   ⚠️ 这里**不能**传 `filtered.length`：缓冲满员（`MAX_UI_LOGS`）之后长度恒为 1500，
 *   这个依赖就再也不变了 → 跟随**静默失效**（新行不再滚进视野）。这是 task-55 顺带
 *   发现并修掉的第二个真 bug，调用方现在传 `${长度}:${最新一行的 seq}`。
 */
export function useFollowScroll(contentRevision: string | number) {
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
  }, [contentRevision, follow]);

  return { boxRef, bottomRef, follow, setFollow, onScroll };
}

/**
 * 缓冲从**前面**裁掉旧行时，把阅读位置钉在原处（跟随关闭、用户正在读中间时）。
 *
 * # 为什么浏览器不替我们做
 *
 * 这正是 CSS 的 scroll anchoring（`overflow-anchor`）要解决的问题，但 **WebKit 长期没有实现它**：
 * <https://bugs.webkit.org/show_bug.cgi?id=171099>（2023 年维护者还明确写「currently not implemented」），
 * 直到 2026-03 那条才被并入 <https://bugs.webkit.org/show_bug.cgi?id=307734>「Enable in stable」而关闭。
 * 本应用要求 **macOS 13+**，跑在 WKWebView 上 —— 大量用户所在系统的 WebKit 没有这个能力，
 * 所以不能把「位置不跳」寄托在引擎上。
 *
 * # 怎么钉（关键：量**残余**位移，而不是假定引擎什么都没做）
 *
 * 记住一个**幸存行**的屏幕位置；每次 DOM 更新后量它的新位置，把差值反向加到 `scrollTop`：
 *
 * * 引擎**没**做锚定 → 该行上移了 h → 我们补 h（这就是老 WebKit 上的修复）；
 * * 引擎**做**了锚定 → 该行位置不变 → 差值为 0 → **我们什么都不做**（不会二次补偿）。
 *
 * 所以不需要给容器加 `overflow-anchor: none`，在新旧引擎上都正确。
 *
 * # 只在两件事同时成立时才补偿
 *
 * 1. `enabled`（调用方传「跟随已关闭」）—— 跟随开着时本来就要贴底，补偿会把它从底部拉开；
 * 2. **头部序号变了**（真的发生了前面的裁剪）—— 过滤/切换等级导致的行增删不补偿，
 *    那种情况下用户本来就期望视图变化。
 *
 * @param headKey 当前第一条日志的 `seq`（没有日志时传 null）
 */
export function usePreserveReadingPosition(
  boxRef: RefObject<HTMLElement | null>,
  enabled: boolean,
  headKey: number | null,
): void {
  const anchorRef = useRef<{ el: Element; top: number } | null>(null);
  const headRef = useRef<number | null>(null);

  // 用户滚动时**必须**刷新基准，否则补偿会多算一个「用户自己滚走的距离」。
  //
  // 为什么不能只靠渲染时机刷新：显式关掉跟随后 `onScroll` 会提前返回（不许被位置翻案），
  // 于是滚动**不引起任何 state 变化 → 不重渲染** → 留在 effect 里的旧基准就是滚之前的值。
  // 真实浏览器里程序性/用户滚动都会发 `scroll` 事件，所以监听它最可靠。
  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const refresh = () => {
      const a = anchorRef.current;
      if (a && a.el.isConnected) a.top = a.el.getBoundingClientRect().top;
    };
    box.addEventListener("scroll", refresh);
    return () => box.removeEventListener("scroll", refresh);
  }, [boxRef]);

  useLayoutEffect(() => {
    const box = boxRef.current;
    if (!box) return;

    const trimmed = headRef.current !== null && headKey !== null && headRef.current !== headKey;
    headRef.current = headKey;

    const anchor = anchorRef.current;
    if (anchor && anchor.el.isConnected) {
      const top = anchor.el.getBoundingClientRect().top;
      if (trimmed && enabled) {
        // 把锚点**挪回**它原来的屏幕位置：补偿量就是它这一帧的位移。
        box.scrollTop += top - anchor.top;
        // 补偿之后它的真实位置已经回到 `anchor.top`，所以基准**保持不变**
        // （写成 `anchor.top = top` 会让下一次的位移算成 0，补偿只生效一次）。
      } else {
        // 没补偿（跟随开着 / 不是裁剪引起的更新）→ 基准跟随实际位置。
        anchor.top = top;
      }
      return;
    }

    // 锚点没了（首帧、或它终于被裁掉）：改用**最后一行**。
    // 为什么是最后一行而不是第一行：裁剪只从**前面**发生，最后一行能活最久
    // （第一行正是下一条要被裁掉的那一行，用它当锚点每次都会失效、丢掉一次补偿）。
    const all = box.querySelectorAll("[data-log-seq]");
    const el = all[all.length - 1];
    anchorRef.current = el ? { el, top: el.getBoundingClientRect().top } : null;
  });
}
