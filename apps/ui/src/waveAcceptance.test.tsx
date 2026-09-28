/**
 * task-66 反证：不放心「作者的测试」，自己构造场景打这一处修复。
 *
 * 本文件只做**攻击**，不复跑作者的用例。目标是**日志阅读位置**
 * （task-55 `ee602d9`）：`usePreserveReadingPosition` 只在「跟随关闭 + 头部序号
 * 变了（真的发生裁剪）」时补偿。我要打的是它**不该动**和**该动**的边界：
 * 过滤/搜索引起的行增删、跟随开着、锚点被裁掉。
 *
 * 注意：这些是**特征化**断言 —— 我把今天的行为（含缺口）钉住，而不是假定它是对的。
 */

import { cleanup, renderHook } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { usePreserveReadingPosition } from "./useFollowScroll";

// ---------------------------------------------------------------------------
// 1) 日志阅读位置：自己搭一个可控的滚动容器
// ---------------------------------------------------------------------------

const realGBCR = Element.prototype.getBoundingClientRect;

/** 每个元素一个可控的 `top`（jsdom 没有布局）。 */
let tops = new WeakMap<Element, number>();

function installFakeRects(): void {
  Element.prototype.getBoundingClientRect = function (this: Element): DOMRect {
    const top = tops.get(this) ?? 0;
    return { top, bottom: top + 20, left: 0, right: 100, width: 100, height: 20, x: 0, y: top, toJSON: () => ({}) } as DOMRect;
  };
}

interface Box {
  el: HTMLDivElement;
  rows: HTMLDivElement[];
}

/** 造一个「有 3 行日志」的滚动容器：scrollTop 可写、尺寸自定。 */
function makeBox(): Box {
  const el = document.createElement("div");
  Object.defineProperty(el, "scrollHeight", { value: 1000, configurable: true });
  Object.defineProperty(el, "clientHeight", { value: 200, configurable: true });
  el.scrollTop = 500;
  const rows = [0, 1, 2].map((i) => {
    const r = document.createElement("div");
    r.setAttribute("data-log-seq", String(i + 1));
    el.append(r);
    return r;
  });
  document.body.append(el);
  return { el, rows };
}

function useHarness(box: HTMLDivElement, props: { enabled: boolean; head: number | null }): void {
  const ref = useRef<HTMLElement | null>(box);
  usePreserveReadingPosition(ref, props.enabled, props.head);
}

beforeEach(() => {
  tops = new WeakMap();
  installFakeRects();
});
afterEach(() => {
  cleanup();
  Element.prototype.getBoundingClientRect = realGBCR;
  document.body.innerHTML = "";
});

describe("反证 1：日志阅读位置（usePreserveReadingPosition）", () => {
  it("A. 头部序号没变（过滤/搜索引起的重渲染）→ 一个像素都不许动", () => {
    const { el, rows } = makeBox();
    const { rerender } = renderHook((p) => useHarness(el, p), {
      initialProps: { enabled: true, head: 1 },
    });
    // 过滤只改变**渲染哪些行**，`headKey`（= 未过滤缓冲的第一条）不变
    tops.set(rows[2]!, -40); // 模拟过滤后可见内容整体上移
    rerender({ enabled: true, head: 1 });
    expect(el.scrollTop, "过滤/搜索引起的更新被误当成裁剪补偿了").toBe(500);
  });

  it("B. 真的发生裁剪（head 变了）→ 必须把阅读位置补回去", () => {
    const { el, rows } = makeBox();
    const { rerender } = renderHook((p) => useHarness(el, p), {
      initialProps: { enabled: true, head: 1 },
    });
    // 前面裁掉一行 → 幸存内容整体上移 40px（top 从 0 变成 -40）
    tops.set(rows[2]!, -40);
    rerender({ enabled: true, head: 2 });
    // 补偿 = top - anchor.top = -40 - 0 → scrollTop 500 → 460，锚点回到原屏幕位置
    expect(el.scrollTop, "裁剪时没有补偿 —— 用户正读的位置会漂走").toBe(460);
  });

  it("C. 跟随开着（enabled=false）→ 不补偿（否则会把视图从底部拉开）", () => {
    const { el, rows } = makeBox();
    const { rerender } = renderHook((p) => useHarness(el, p), {
      initialProps: { enabled: false, head: 1 },
    });
    tops.set(rows[2]!, -40);
    rerender({ enabled: false, head: 2 });
    expect(el.scrollTop, "跟随开着时不该补偿").toBe(500);
  });

  it("D. 锚点那一行自己也被裁掉 → 本帧不补偿（特征化：这是已知缺口，不是崩溃）", () => {
    const { el, rows } = makeBox();
    const { rerender } = renderHook((p) => useHarness(el, p), {
      initialProps: { enabled: true, head: 1 },
    });
    // 锚点 = 渲染列表的**最后一行**；若它正好是「最旧的一行」而这一帧被裁掉：
    rows[2]!.remove();
    tops.set(rows[1]!, -40);
    rerender({ enabled: true, head: 2 });
    expect(el.scrollTop, "锚点消失时本帧没有可用的补偿基准").toBe(500);
  });
});

