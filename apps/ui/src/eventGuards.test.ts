/**
 * 事件载荷校验的回归测试。
 *
 * 这些校验防的是一次**实测发生过的崩溃**：Tauri 事件的载荷不是数组时，
 * 处理器里的 `for...of` 抛 `results is not iterable`，React 整棵树被卸载 ——
 * 用户看到界面凭空消失。所以「形状不对就忽略」这个行为值得钉住。
 */

import { afterEach, describe, expect, it, vi } from "vitest";

import { isCount, isObject, isText, rejectPayload, resetWarnings } from "./eventGuards";

describe("事件载荷校验", () => {
  afterEach(() => {
    resetWarnings();
    vi.restoreAllMocks();
  });

  it("isObject 只接受普通对象", () => {
    expect(isObject({})).toBe(true);
    expect(isObject({ a: 1 })).toBe(true);
    // 这些都不该被当成对象：null 与数组会让属性访问与遍历出意外
    expect(isObject(null)).toBe(false);
    expect(isObject(undefined)).toBe(false);
    expect(isObject([])).toBe(false);
    expect(isObject("x")).toBe(false);
    expect(isObject(1)).toBe(false);
  });

  it("isText 只接受字符串", () => {
    expect(isText("")).toBe(true);
    expect(isText("a")).toBe(true);
    expect(isText(1)).toBe(false);
    expect(isText(null)).toBe(false);
    expect(isText(undefined)).toBe(false);
  });

  it("isCount 拒掉 NaN 与 Infinity（它们会被当成合法数值一路带进界面）", () => {
    expect(isCount(0)).toBe(true);
    expect(isCount(-1.5)).toBe(true);
    expect(isCount(NaN)).toBe(false);
    expect(isCount(Infinity)).toBe(false);
    expect(isCount("10")).toBe(false);
    expect(isCount(undefined)).toBe(false);
  });

  it("形状不对时记一次原因，同一个事件不重复刷屏", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    rejectPayload("nodes://latency", [1, 2], "载荷不是数组");
    rejectPayload("nodes://latency", "again", "载荷不是数组");

    expect(spy).toHaveBeenCalledTimes(1);
    expect(String(spy.mock.calls[0]?.[0])).toContain("nodes://latency");
    expect(String(spy.mock.calls[0]?.[0])).toContain("载荷不是数组");
  });

  it("不同事件各自提示一次", () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    rejectPayload("core://log", {}, "缺少 line");
    rejectPayload("update://progress", {}, "缺少 label");
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it("rejectPayload 返回 void，调用点可以写成 return rejectPayload(...)", () => {
    vi.spyOn(console, "error").mockImplementation(() => {});
    expect(rejectPayload("x", null, "y")).toBeUndefined();
  });
});
