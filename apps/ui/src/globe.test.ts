/**
 * 地球仪视角计算的回归测试。
 *
 * 这几条逻辑（对准哪里、缩放多少）算错的后果是**视角偏到看不见航线**，
 * 而截图只能看到「某一刻、某一组数据」的样子。所以把它们做成纯函数测。
 */

import { describe, expect, it } from "vitest";

import {
  MAX_ZOOM,
  MIN_ZOOM,
  ROTATE_MAX_SPAN_RAD,
  clampZoom,
  fitZoom,
  focusPoint,
  viewFor,
} from "./pages/Globe";

/** 与 `Globe.tsx` 同一套换算（角度 → 弧度）。 */
const DEG = Math.PI / 180;
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
    route: { from, to, bytes, node_name: "n", traffic_ok: true, counter_resets: 0 },
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
  /**
   * `fitZoom` 的两端行为：**近处要放大到两点分得开，远处要缩小到两点都进画面**。
   *
   * 这条测试曾经断言的是「近处保持 zoom=1（不放大）」—— 那是**我修过头**的产物：
   * 为防「129 公里航线被放大到只剩一片海岸」，我写成 `min(1, ...)`，结果近距离
   * 航线恒为 1、两点在屏幕上只相距 4.78px（用户反馈的「放大倍数不够」）。
   *
   * 现在改为取「装得下所需（≤1）」与「分得开所需（>1）」中较大的那个：
   * 近处用放大值、远处用缩小值，二者在 θ≈40° 处都等于 1，所以是连续的。
   */
  it("按距离切换：<40° 放大让两点分得开，>40° 缩小让两点都进画面", () => {
    // 广州 ↔ 香港（129 km，1.163°）—— 用户实际使用的那组
    const sameCity = fitZoom(data(loc(23.13, 113.27), loc(22.32, 114.17), 1));
    // 广州 → 新加坡（约 26°）
    const crossSea = fitZoom(data(loc(23.13, 113.27), loc(1.35, 103.8), 1));
    // 广州 → 纽约（约 135°，必须缩小才能同时看到两端）
    const crossGlobe = fitZoom(data(loc(23.13, 113.27), loc(40.7, -74.0), 1));

    // **近处必须放大**：屏幕距离要够看（这是本次修复的核心）
    expect(sameCity).toBeGreaterThan(1);
    // 广州↔香港角距 0.0203 rad，目标 0.7 rad → zoom≈34.5
    // 屏幕上两点距离 = 2·0.42·canvas·zoom·sin(θ/2)，canvas=720 时约 212px
    expect(sameCity).toBeGreaterThan(20);
    expect(sameCity).toBeLessThanOrEqual(MAX_ZOOM);

    // 中距离（26° < 40° 的交接点）：仍然偏放大，让两点分得开
    expect(crossSea).toBeGreaterThan(1);
    expect(crossSea).toBeLessThan(sameCity);
    // 跨半球（135° > 40°）：必须缩小，否则两端里有一个会在画面外
    expect(crossGlobe).toBeLessThan(1);
    expect(crossSea).toBeGreaterThan(crossGlobe);

    // **单调**：越远越小（交接点 0.7 rad ≈ 40° 两侧分别放大与缩小）
    expect(sameCity).toBeGreaterThan(crossSea);
    expect(crossSea).toBeGreaterThan(crossGlobe);

    for (const z of [sameCity, crossSea, crossGlobe]) {
      expect(z).toBeGreaterThanOrEqual(MIN_ZOOM);
      expect(z).toBeLessThanOrEqual(MAX_ZOOM);
    }
  });

  /**
   * **可读性断言**：把 zoom 换算成「两点屏幕距离」，直接钉住用户能看到什么。
   *
   * 这条是为了防止「公式改了但观感没变」——只断言 zoom 的数值不够，
   * 因为观感取决于 zoom 与角距的乘积。UI 设计实测修复前 zoom=1 时只有 4.78px。
   */
  it("广州↔香港在默认缩放下两点应当分得开（≥150px）", () => {
    const CANVAS = 720; // Globe.tsx 的 canvas 内部尺寸
    const R = Math.min(CANVAS, CANVAS) * 0.42;
    const from = loc(23.1317, 113.266);
    const to = loc(22.3193, 114.169);
    const a = [Math.cos(from.lat * DEG) * Math.sin(from.lon * DEG), Math.sin(from.lat * DEG),
               Math.cos(from.lat * DEG) * Math.cos(from.lon * DEG)] as const;
    const b = [Math.cos(to.lat * DEG) * Math.sin(to.lon * DEG), Math.sin(to.lat * DEG),
               Math.cos(to.lat * DEG) * Math.cos(to.lon * DEG)] as const;
    const dot = a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    const theta = Math.acos(Math.max(-1, Math.min(1, dot)));
    const z = fitZoom(data(from, to, 1));
    const screenPx = 2 * R * z * Math.sin(theta / 2);

    // 修复前：zoom=1 → 4.78px（两点几乎重合，标签还把它们盖住）
    expect(screenPx).toBeGreaterThan(150);
    expect(screenPx).toBeLessThan(600); // 也不该把两点甩出画布
  });

  it("两点重合时不会除以零（保持整球可见）", () => {
    const same = loc(23.13, 113.27);
    const z = fitZoom(data(same, same, 1));
    expect(Number.isFinite(z)).toBe(true);
    expect(z).toBe(1);
  });
});

describe("地球仪视角：居中与自转", () => {
  /**
   * **防「整屏空白」的回归测试。**
   *
   * 真事：`view.lat` 早先写成 `focus.lat * DEG * 0.6`，纬度旋转不到位。
   * zoom=1 时只偏 48px 看不出来，但放大到 34.5× 后焦点被推到画布上方
   * 1287px —— 整屏空白，标记与航线全在视野外。
   *
   * 这里用与 `Globe.tsx` 相同的旋转语义算出「焦点投影到哪」，
   * 要求它落在画布中心。**乘任何系数都会让这条测试红。**
   */
  it("焦点必须投影到画布中心（不能乘 0.6 之类的系数）", () => {
    const W = 720, R0 = Math.min(W, W) * 0.42;
    const data0: GlobeData = {
      route: {
        from: loc(23.1317, 113.266),
        to: loc(22.3193, 114.169),
        bytes: 9_000_000,
        node_name: "n",
        traffic_ok: true,
        counter_resets: 0,
      },
      origin: loc(23.1317, 113.266),
      error: null,
    };
    const v = viewFor(data0);
    expect(v).not.toBeNull();
    // 复刻 rotY / rotX / toVec（与 Globe.tsx 同一套）
    const toV = (lat: number, lon: number) => {
      const la = lat * DEG, lo = lon * DEG;
      return [Math.cos(la) * Math.sin(lo), Math.sin(la), Math.cos(la) * Math.cos(lo)];
    };
    const rotY = (p: number[], a: number) => {
      const c = Math.cos(a), s = Math.sin(a);
      return [p[0]! * c + p[2]! * s, p[1]!, -p[0]! * s + p[2]! * c];
    };
    const rotX = (p: number[], a: number) => {
      const c = Math.cos(a), s = Math.sin(a);
      return [p[0]!, p[1]! * c - p[2]! * s, p[1]! * s + p[2]! * c];
    };
    // 焦点的经纬：焦点定义在中点
    const focus = focusPoint(data0)!;
    for (const zoom of [1, 8, v!.zoom]) {
      const R = R0 * zoom;
      const r = rotX(rotY(toV(focus.lat, focus.lon), v!.lon), v!.lat);
      const x = W / 2 + r[0]! * R, y = W / 2 - r[1]! * R;
      expect(Math.abs(x - W / 2)).toBeLessThan(1);
      expect(Math.abs(y - W / 2)).toBeLessThan(1);
    }
  });

  /**
   * **防「放大后自转把视野扫走」。**
   *
   * 真事：`auto` 恒为 true，放大到 34.5×（R=10424）后自转 13.5° 让两点
   * 横向偏出 2500px，整屏空白。所以跨得越近、放得越大时必须关掉自转。
   */
  it("近距离航线放大后必须关闭自转，远距离仍可自转", () => {
    const near = viewFor({
      route: { from: loc(23.1317, 113.266), to: loc(22.3193, 114.169), bytes: 1, node_name: "n",
               traffic_ok: true, counter_resets: 0 },
      origin: loc(23.1317, 113.266), error: null,
    })!;
    const far = viewFor({
      route: { from: loc(23.13, 113.27), to: loc(40.7, -74.0), bytes: 1, node_name: "n",
               traffic_ok: true, counter_resets: 0 },
      origin: loc(23.13, 113.27), error: null,
    })!;

    expect(near.auto).toBe(false);      // 129km、放大到 34.5× → 不能自转
    expect(far.auto).toBe(true);        // 跨半球、缩小 → 自转无妨
    expect(near.zoom).toBeGreaterThan(far.zoom);
    expect(ROTATE_MAX_SPAN_RAD).toBeGreaterThan(0);
  });

  /** 没有航线时按「看整球」处理，自转无妨，且不能抛错。 */
  it("没有航线时不抛错、允许自转", () => {
    const v = viewFor({ route: null, origin: loc(10, 20), error: null });
    expect(v).not.toBeNull();
    expect(v!.auto).toBe(true);
  });
});
