/**
 * task-66 反证：不放心「作者的测试」，自己构造场景打这两处修复。
 *
 * 本文件只做**攻击**，不复跑作者的用例。两条目标：
 *
 * 1. **日志阅读位置**（task-55 `ee602d9`）：`usePreserveReadingPosition` 只在
 *    「跟随关闭 + 头部序号变了（真的发生裁剪）」时补偿。我要打的是它**不该动**
 *    和**该动**的边界：过滤/搜索引起的行增删、跟随开着、锚点被裁掉。
 * 2. **恢复期可见性**（task-60）：后端在 `recovery.begin` 之前就会推
 *    `probe_failures: 1/2`。用后端会发的**字段序列**走一遍，看界面每个阶段到底显示什么。
 *
 * 注意：这些是**特征化**断言 —— 我把今天的行为（含缺口）钉住，而不是假定它是对的。
 */

import { act, cleanup, renderHook } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { recoveryView } from "./ipc";
import { usePreserveReadingPosition } from "./useFollowScroll";
import type { RecoveryState } from "./types";

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

// ---------------------------------------------------------------------------
// 2) 恢复期可见性：用后端会发的字段序列走一遍
// ---------------------------------------------------------------------------

function rec(over: Partial<RecoveryState>): RecoveryState {
  return {
    recovering: false,
    attempt: 0,
    probe_failures: 0,
    started_unix: null,
    last_outcome: null,
    finished_unix: null,
    ...over,
  } as RecoveryState;
}

describe("反证 2：自愈窗口的可见性（recoveryView）", () => {
  it("后端字段序列：探到失败 → 重建 → 成功/失败，界面各阶段显示什么（证据）", () => {
    const seq: { step: string; state: RecoveryState }[] = [
      { step: "平时（running）", state: rec({ probe_failures: 0 }) },
      { step: "首次探测失败（t≈16s，后端会推 failures=1）", state: rec({ probe_failures: 1 }) },
      { step: "第二次失败、即将重建（failures=2）", state: rec({ probe_failures: 2 }) },
      { step: "正在重建", state: rec({ recovering: true, attempt: 1, probe_failures: 2 }) },
      { step: "重建成功", state: rec({ attempt: 1, last_outcome: "recovered" }) },
      // 退回直连之后核心已经停了 → 这一步必须是 running=false（真实状态）
      { step: "重建失败、退回直连（此时 running=false）", state: rec({ attempt: 1, last_outcome: "direct_fallback" }) },
    ];
    for (const { step, state } of seq) {
      const v = recoveryView(state, !/running=false/.test(step));
      console.log(`[恢复期文案] ${step} → phase=${v.phase} text=${JSON.stringify(v.text)} button=${v.button}`);
    }
    // 成功/失败两种结局都必须可感知（这条在 HEAD 与当前工作区都成立）
    expect(recoveryView(rec({ attempt: 1, last_outcome: "recovered" }), true).justRecovered).toBe(true);
    expect(recoveryView(rec({ attempt: 1, last_outcome: "direct_fallback" }), false).text).toContain("退回直连");
  });

  it("【要求】probe_failures ≥ 1 时界面必须有可见信号（HEAD bdeee46 上是红的 —— 那就是缺口）", () => {
    // 后端在 `recovery.begin` **之前**就会推 failures=1/2（`sync_probe_failures`）。
    // 看门狗 10s 间隔 + 6s 超时 + 连续 2 次 ⇒ 从首次失败到开始重建约 16 秒，
    // 这 16 秒里整机已经断网，界面必须给用户一个可见的动作（至少提示「先断开」）。
    const silent: string[] = [];
    for (const n of [1, 2]) {
      const v = recoveryView(rec({ probe_failures: n }), true);
      if (v.text === null || v.text === "") silent.push(`probe_failures=${n}`);
      else console.log(`[可见] probe_failures=${n} → ${JSON.stringify(v.text)}`);
    }
    expect(silent, "这些阶段界面是沉默的（用户整机断网却看到「一切正常」）").toEqual([]);
  });
});
