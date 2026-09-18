/**
 * 「跟随滚动」的回归测试。
 *
 * 重点钉住的是用户实测到的问题：**跟随开着时手动往上翻，下一条日志会把你
 * 拽回底部**（实测 scrollTop 50 → 2065）。修好之后，往上滚应当自动暂停跟随。
 *
 * 关于 jsdom：它不做真实布局，所以容器的 `clientHeight` / `scrollHeight`
 * 由测试自己定义，`scrollTop` 赋值会原样保留 —— 这正好够验证「谁把 scrollTop
 * 改成了多少」这类行为。
 */

import { act, fireEvent, render } from "@testing-library/react";
import { useRef, useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { BOTTOM_SLACK_PX, atBottom, useFollowScroll } from "./useFollowScroll";

/** 给滚动容器一个确定的尺寸（jsdom 默认全是 0）。 */
function sizeBox(el: HTMLElement, height = 400, content = 2000) {
  Object.defineProperty(el, "clientHeight", { value: height, configurable: true });
  Object.defineProperty(el, "scrollHeight", { value: content, configurable: true });
}

/** 用按钮驱动「新内容到达」，避免在测试体里重复 render 出两棵树。 */
function Harness({ initial = 10 }: { initial?: number }) {
  const [content, setContent] = useState(initial);
  const { boxRef, bottomRef, follow, setFollow, onScroll } = useFollowScroll(content);
  // 把 ref 同时挂到测试可读的地方
  const local = useRef<HTMLDivElement | null>(null);
  return (
    <div>
      <label>
        <input
          type="checkbox"
          checked={follow}
          onChange={(e) => setFollow(e.target.checked)}
          data-testid="follow"
        />
        跟随
      </label>
      <button data-testid="more" onClick={() => setContent((c) => c + 1)}>
        新日志
      </button>
      <div
        data-testid="box"
        ref={(el) => {
          boxRef.current = el;
          local.current = el;
        }}
        onScroll={onScroll}
      >
        <div data-testid="bottom" ref={bottomRef} />
      </div>
    </div>
  );
}

describe("useFollowScroll", () => {
  beforeEach(() => {
    // jsdom 不实现 scrollIntoView；兜底分支要用到它。
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("atBottom 在贴着底部时为真，并有少量容差", () => {
    expect(atBottom({ scrollHeight: 1000, clientHeight: 400, scrollTop: 600 })).toBe(true);
    // 差 8px 以内仍算在底部（亚像素与圆整）
    expect(
      atBottom({ scrollHeight: 1000, clientHeight: 400, scrollTop: 600 - BOTTOM_SLACK_PX }),
    ).toBe(true);
    expect(atBottom({ scrollHeight: 1000, clientHeight: 400, scrollTop: 500 })).toBe(false);
  });

  it("跟随开着时，新内容会滚到底", () => {
    const { getByTestId } = render(<Harness />);
    const box = getByTestId("box");
    sizeBox(box);
    box.scrollTop = 0;

    fireEvent.click(getByTestId("more"));
    expect(box.scrollTop).toBe(box.scrollHeight);
  });

  it("跟随关闭时不滚 —— 这是开关最基本的语义", () => {
    const { getByTestId } = render(<Harness />);
    const box = getByTestId("box");
    sizeBox(box);

    fireEvent.click(getByTestId("follow"));
    expect((getByTestId("follow") as HTMLInputElement).checked).toBe(false);

    box.scrollTop = 123;
    fireEvent.click(getByTestId("more"));
    expect(box.scrollTop).toBe(123);
  });

  it("**跟随开着时手动往上滚，应当自动暂停跟随**（用户实测的问题）", () => {
    const { getByTestId } = render(<Harness />);
    const box = getByTestId("box");
    sizeBox(box);
    const checkbox = getByTestId("follow") as HTMLInputElement;
    expect(checkbox.checked).toBe(true);

    // 用户往上翻历史
    act(() => {
      box.scrollTop = 50;
      fireEvent.scroll(box);
    });

    // 跟随应当自动暂停（开关同步变关）
    expect(checkbox.checked).toBe(false);

    // 此时新日志到达，**不能**把视图拽回底部
    box.scrollTop = 50;
    fireEvent.click(getByTestId("more"));
    expect(box.scrollTop).toBe(50);
  });

  it("滚回底部时自动恢复跟随", () => {
    const { getByTestId } = render(<Harness />);
    const box = getByTestId("box");
    sizeBox(box);
    const checkbox = getByTestId("follow") as HTMLInputElement;

    act(() => {
      box.scrollTop = 50;
      fireEvent.scroll(box);
    });
    expect(checkbox.checked).toBe(false);

    act(() => {
      box.scrollTop = box.scrollHeight - box.clientHeight; // 回到底部
      fireEvent.scroll(box);
    });
    expect(checkbox.checked).toBe(true);
  });
});
