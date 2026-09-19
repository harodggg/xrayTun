/**
 * 地球仪视角计算的回归测试。
 *
 * 这几条逻辑（对准哪里、缩放多少）算错的后果是**视角偏到看不见航线**，
 * 而截图只能看到「某一刻、某一组数据」的样子。所以把它们做成纯函数测。
 */

import { describe, expect, it } from "vitest";

import { MAX_ZOOM, MIN_ZOOM, clampZoom, fitZoom, focusPoint } from "./pages/Globe";
import type { GlobeData, GeoLocation } from "./types";

function loc(lat: number, lon: number): GeoLocation {
  return {
    ip: "1.2.3.4",
    country: "X",
    city: "Y",
    lat,
    lon,
    isp: "",
    source: "test",
    consistent: true,
    sources: ["test"],
  };
}

function data(from: GeoLocation, to: GeoLocation, bytes: number): GlobeData {
  return {
    route: { from, to, bytes, node_name: "n" },
    origin: from,
    error: null,
  };
}

describe("地球仪视角", () => {
  it("clampZoom 把缩放限制在可用范围内", () => {
    // 引用常量而不是写死数值：上限调整过两次，硬编码会让测试无意义地红
    expect(clampZoom(0.01)).toBe(MIN_ZOOM);
    expect(clampZoom(99)).toBe(MAX_ZOOM);
    expect(MAX_ZOOM).toBeGreaterThanOrEqual(4); // 用户要求能放到城市级
    expect(clampZoom(1.4)).toBeCloseTo(1.4);
  });

  it("有流量时对准两端的中点（不是某一端）", () => {
    const d = data(loc(23.13, 113.27), loc(22.32, 114.17), 9_000_000);
    const f = focusPoint(d);
    expect(f).not.toBeNull();
    expect(f!.lat).toBeCloseTo((23.13 + 22.32) / 2, 5);
    expect(f!.lon).toBeCloseTo((113.27 + 114.17) / 2, 5);
  });

  it("没有流量数据时退到起点，而不是给一个随机经度", () => {
    const d = data(loc(25.0, 100.0), loc(22.0, 114.0), 0);
    const f = focusPoint(d);
    expect(f!.lat).toBeCloseTo(25.0, 5);
    expect(f!.lon).toBeCloseTo(100.0, 5);
  });

  it("航线缺失时也能给出一个位置（只有本机位置）", () => {
    const d: GlobeData = { route: null, origin: loc(10, 20), error: null };
    expect(focusPoint(d)).toEqual({ lat: 10, lon: 20 });
  });

  /**
   * 关键行为：**跨得越远，缩放越小**（要缩小才能同时看到两端）。
   * 这条如果反了，屏幕上就只剩一个点、看不见航线。
   */
  it("短航线默认整球可见（不放大），跨得越远越小", () => {
    // 广州 ↔ 香港：同片区域，应当保持整球可见
    const sameCity = fitZoom(data(loc(23.13, 113.27), loc(22.32, 114.17), 1));
    // 广州 → 新加坡（约 26°）
    const crossSea = fitZoom(data(loc(23.13, 113.27), loc(1.35, 103.8), 1));
    // 广州 → 纽约（约 135°，必须缩小才能同时看到两端）
    const crossGlobe = fitZoom(data(loc(23.13, 113.27), loc(40.7, -74.0), 1));

    // **默认不放大**：这正是用户要的「合适的观看位置」——先看全地球
    expect(sameCity).toBe(1);
    expect(crossSea).toBeLessThanOrEqual(1);
    // 跨半球必须明显缩小，否则两端里有一个会在画面外
    expect(crossGlobe).toBeLessThan(1);
    expect(crossSea).toBeGreaterThanOrEqual(crossGlobe);

    for (const z of [sameCity, crossSea, crossGlobe]) {
      expect(z).toBeGreaterThanOrEqual(MIN_ZOOM);
      expect(z).toBeLessThanOrEqual(MAX_ZOOM);
    }
  });

  it("两点重合时不会除以零（保持整球可见）", () => {
    const same = loc(23.13, 113.27);
    const z = fitZoom(data(same, same, 1));
    expect(Number.isFinite(z)).toBe(true);
    expect(z).toBe(1);
  });
});
