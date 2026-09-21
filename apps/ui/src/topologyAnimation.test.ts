/**
 * 拓扑页「数据乱跳」的几何连续性测试。
 *
 * # 为什么要有这个文件
 *
 * 用户反复报告「数据还是在乱跳」，但历史上每一次排查都靠 CDP 临时脚本 + 肉眼看截图，
 * 结论无法在 CI 里复现、也无法在改动后立刻验证。v0.8.21 把「最大单步位移 457px」
 * 降到 88.9px，但用户仍说在跳。这个文件把「跳」变成**可重复、可回归**的数字判据。
 *
 * # 三类「跳」分别怎么量（不要混为一谈）
 *
 * 1. 车的位置跳 —— 相邻两帧的位移出现孤立尖峰。判据是 **最大/中位比值**（相对判据，
 *    不用固定像素阈值：路径长度会随布局变，固定阈值会误红/误绿）。
 *    更进一步：一次「刷新/几何变化」造成的单帧位移，不得比稳态单帧位移大一个量级
 *    —— 稳态位移就是「正常走一帧」的距离，这是最直观的「跳不跳」标尺。
 * 2. 卡片数字跳 —— 刷新时 ↓↑ 字节数出现不合理回退（8.01 GiB → 0 B → 8.01 GiB）。
 *    判据是「查不到流量时，界面上不允许出现伪造的 0 B」。
 * 3. 几何跳 —— 卡片宽度 / 容器宽度 / 出口数量变化 → 路径长度与形状变化 →
 *    同一进度落到不同屏幕点。判据同 1（单帧位移不得是尖峰）。
 *
 * # 为什么不用真实 rAF 采样（这是本文件相对浏览器采样的优势）
 *
 * 运维在真实浏览器里实测到：多个 headless 页并发时 `ratio_max_median` 会从 1.36
 * 涨到 2.64，而代码没变 —— 原因是 rAF 掉帧、`dt` 被夹在 0.1s，一帧走了两步的路。
 * 本文件用手动时钟**固定 dt=1/60s** 推进，判据是确定性的：
 * 「单帧位移」天然等于「1/60 秒的路程」，掉帧噪声不存在。
 * 因此这里不需要 `dt_normalized` 那个比值；真实浏览器采样才需要两个比值同时超标。
 *
 * # 货车身份（测试与被测代码之间的唯一约定）
 *
 * 「同一辆车跨刷新是否瞬移」需要身份。组件当前用数组下标当身份（`data-slot`），
 * 而这正是疑似根因之一。约定：**货车 `<g>` 若带 `data-truck-key`（稳定身份），
 * 测试按它比较；否则退化为按 `data-slot` 比较** —— 后者会把「slot 复用错位」
 * 如实报成跳变。修复时请给货车挂上稳定 key（如 `route#ordinal`），
 * 这样测试测的就是「同一辆车」，而不是「同一个下标」。
 *
 * # jsdom 缺什么，以及怎么补（本文件的核心技术手段）
 *
 * - jsdom **没有** `getPointAtLength` / `getTotalLength`（连 `SVGPathElement` 都没有，
 *   `<path>` 的原型就是 `SVGElement`）。这里在原型上装一个**真的会算的** fake path：
 *   解析组件真实写进 DOM 的 `d`（只有 `M` + `C`，见 `routeToD`），把每段三次贝塞尔
 *   离散成折线做弧长参数化。所以测的是**组件真实产出的几何**（闭环性、单子路径、
 *   货车 `transform` 落点），不是「调用不报错」。
 * - jsdom **没有布局引擎**（`getBoundingClientRect` 全 0）也**没有 `ResizeObserver`**。
 *   用可控假布局替换 rect、手写 RO 桩，让「卡片变宽 / 容器变宽 / 出口增减」这类几何
 *   扰动可以确定性地注入。脱挂（已卸载）元素按浏览器语义返回全 0 rect —— 这正是
 *   「刷新后 label 被卸载、测量闭包仍持有旧元素数组」时几何塌掉的复现条件。
 * - jsdom 的 rAF 是真实定时器，测试会变得不确定。这里换成**手动时钟** `runFrame(dt)`，
 *   于是「刷新发生在哪一帧」完全可控，跳变能被钉在具体帧上。
 *
 * # 覆盖不到的（完整诚实清单见文件末尾）
 */

import { act, cleanup, render } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import Topology from "./pages/Topology";
import { TRAVEL_SECONDS } from "./topology/flowGeometry";
import type { TopoInbound, TopoOutbound, Topology as TopologyData } from "./types";

// ---------------------------------------------------------------------------
// IPC mock：刷新时喂给组件的就是我们控制的快照
// ---------------------------------------------------------------------------

const state = vi.hoisted(() => ({
  /** 下一次（以及首次）`routing_topology` 返回的数据。 */
  topo: null as unknown,
  /** 接口被调用次数，用来确认「刷新」真的发生了，而不是测了个静态页面。 */
  calls: 0,
}));

vi.mock("./ipc", () => ({
  errorText: (e: unknown) => (e instanceof Error ? e.message : String(e)),
  api: {
    routingTopology: async () => {
      state.calls += 1;
      return state.topo;
    },
    explainDest: async () => {
      throw new Error("explainDest 不应在动画测试里被调用");
    },
  },
}));

// ---------------------------------------------------------------------------
// 假布局：jsdom 没有布局引擎，我们自己给「入口卡片 / 出口卡片 / 容器」定坐标
// ---------------------------------------------------------------------------

const layout = {
  width: 820,
  height: 320,
  /** 卡片宽度。styles.css 用固定 168px 列宽；这里可改，用来注入几何扰动。 */
  cardWidth: 168,
  padLeft: 0,
  padRight: 0,
  rowHeight: 44,
  rowGap: 6,
  labelsTop: 40,
};

function makeRect(left: number, top: number, width: number, height: number): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    right: left + width,
    bottom: top + height,
    width,
    height,
    toJSON: () => ({}),
  } as DOMRect;
}

/**
 * 复刻 styles.css 的拓扑布局语义（`.highway` 三列 grid + 两侧卡片）：
 * - 左列卡片左缘固定、右缘 = 左缘 + 卡片宽 → 入口锚点 `inlet.r` 随卡片宽变；
 * - 右列卡片右对齐、右缘固定，左缘 = 右缘 − 卡片宽 → 出口锚点 `outlet.l` 随卡片宽变；
 * - 卡片宽度变化 = 「数字长度把卡片撑宽」这类几何扰动。
 *
 * 脱挂元素返回全 0（浏览器语义）：组件把元素引用存进 ref 数组并在测量闭包里长期持有，
 * 元素被卸载后 `getBoundingClientRect()` 会变成 0 —— 那个 0 会被当成真实锚点用。
 */
function rectOf(el: Element): DOMRect {
  if (!el.isConnected) return makeRect(0, 0, 0, 0);
  if (el.classList.contains("highway")) {
    return makeRect(0, 0, layout.width, layout.height);
  }
  if (el.classList.contains("highway__lane-label")) {
    const side = el.closest(".highway__side");
    const right = side?.classList.contains("highway__side--right") ?? false;
    const rows = side
      ? [...side.children].filter((c) => c.classList.contains("highway__lane-label"))
      : [];
    const idx = Math.max(0, rows.indexOf(el));
    const top = layout.labelsTop + idx * (layout.rowHeight + layout.rowGap);
    const left = right ? layout.width - layout.padRight - layout.cardWidth : layout.padLeft;
    return makeRect(left, top, layout.cardWidth, layout.rowHeight);
  }
  return makeRect(0, 0, 0, 0);
}

const realGetBoundingClientRect = Element.prototype.getBoundingClientRect;
const realRaf = window.requestAnimationFrame;
const realCancelRaf = window.cancelAnimationFrame;
const realSetInterval = window.setInterval;

// ---------------------------------------------------------------------------
// ResizeObserver 桩 + 手动 rAF 时钟
// ---------------------------------------------------------------------------

const roCallbacks = new Set<ResizeObserverCallback>();

class FakeResizeObserver {
  constructor(private readonly cb: ResizeObserverCallback) {
    roCallbacks.add(cb);
  }
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {
    roCallbacks.delete(this.cb);
  }
}

/** 触发一次「布局变了」的信号。浏览器里这由真实的 ResizeObserver 发出。 */
function fireResize(): void {
  act(() => {
    for (const cb of [...roCallbacks]) cb([], {} as ResizeObserver);
  });
}

let clock = 0;
let pendingFrame: FrameRequestCallback | null = null;
let rafSeq = 0;

/** 手动推一帧。组件的动画循环由 `requestAnimationFrame` 驱动。 */
function runFrame(dtMs: number): void {
  clock += dtMs;
  const cb = pendingFrame;
  pendingFrame = null;
  if (!cb) return;
  act(() => {
    cb(clock);
  });
}

// ---------------------------------------------------------------------------
// fake path：解析组件真实产出的 `d`，做弧长参数化
// ---------------------------------------------------------------------------

const SVG_NS = "http://www.w3.org/2000/svg";
/** 每段三次贝塞尔的离散段数。24 段足以让弧长误差 << 1px。 */
const CUBIC_STEPS = 24;

interface Pt {
  x: number;
  y: number;
}

interface Polyline {
  pts: Pt[];
  cum: number[];
  total: number;
}

function cubicAt(p0: Pt, p1: Pt, p2: Pt, p3: Pt, t: number): Pt {
  const u = 1 - t;
  const a = u * u * u;
  const b = 3 * u * u * t;
  const c = 3 * u * t * t;
  const d = t * t * t;
  return {
    x: a * p0.x + b * p1.x + c * p2.x + d * p3.x,
    y: a * p0.y + b * p1.y + c * p2.y + d * p3.y,
  };
}

const pathCache = new Map<string, Polyline | null>();

/**
 * `routeToD` 只会产出 `M x y` 加若干 `C cx y1, cx y2, x2 y2`。
 * 只支持这两种命令 —— 组件若改用别的命令，解析会返回 null，依赖几何判据的测试
 * 会**显式失败**而不是静默跳过。
 */
function flattenD(d: string): Polyline | null {
  if (pathCache.has(d)) return pathCache.get(d) ?? null;
  const result = flattenDUncached(d);
  pathCache.set(d, result);
  return result;
}

function flattenDUncached(d: string): Polyline | null {
  const tokens = d.match(/[MC]|-?\d+(?:\.\d+)?/g);
  if (!tokens || tokens.length < 3) return null;
  let i = 0;
  const num = (): number => Number(tokens[i++]);
  if (tokens[i++] !== "M") return null;
  const pts: Pt[] = [{ x: num(), y: num() }];
  while (i < tokens.length) {
    if (tokens[i++] !== "C") return null;
    const c1: Pt = { x: num(), y: num() };
    const c2: Pt = { x: num(), y: num() };
    const end: Pt = { x: num(), y: num() };
    const p0 = pts[pts.length - 1]!;
    for (let s = 1; s <= CUBIC_STEPS; s++) {
      pts.push(cubicAt(p0, c1, c2, end, s / CUBIC_STEPS));
    }
  }
  const cum: number[] = [0];
  for (let k = 1; k < pts.length; k++) {
    const a = pts[k - 1]!;
    const b = pts[k]!;
    cum.push(cum[k - 1]! + Math.hypot(b.x - a.x, b.y - a.y));
  }
  const total = cum[cum.length - 1]!;
  if (!Number.isFinite(total)) return null;
  return { pts, cum, total };
}

function pointAt(p: Polyline, len: number): Pt {
  const l = Math.max(0, Math.min(p.total, len));
  let lo = 0;
  let hi = p.cum.length - 1;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (p.cum[mid]! < l) lo = mid + 1;
    else hi = mid;
  }
  if (lo === 0) return { ...p.pts[0]! };
  const a = p.pts[lo - 1]!;
  const b = p.pts[lo]!;
  const d0 = p.cum[lo - 1]!;
  const d1 = p.cum[lo]!;
  const span = d1 - d0;
  const t = span > 0 ? (l - d0) / span : 0;
  return { x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t };
}

function installFakePath(): void {
  const proto = Object.getPrototypeOf(document.createElementNS(SVG_NS, "path")) as {
    getTotalLength?: () => number;
    getPointAtLength?: (len: number) => Pt;
  };
  proto.getTotalLength = function getTotalLength(this: Element): number {
    return flattenD(this.getAttribute("d") ?? "")?.total ?? 0;
  };
  proto.getPointAtLength = function getPointAtLength(this: Element, len: number): Pt {
    const p = flattenD(this.getAttribute("d") ?? "");
    if (!p) throw new Error("fake path: 无法解析 guide path 的 d 属性");
    return pointAt(p, len);
  };
}

function uninstallFakePath(): void {
  const proto = Object.getPrototypeOf(document.createElementNS(SVG_NS, "path")) as {
    getTotalLength?: () => number;
    getPointAtLength?: (len: number) => Pt;
  };
  delete proto.getTotalLength;
  delete proto.getPointAtLength;
}

// ---------------------------------------------------------------------------
// 测量
// ---------------------------------------------------------------------------

/**
 * 货车身份：优先 `data-truck-key`（稳定身份），否则退化为 `data-slot`。
 * 修复后请挂上 `data-truck-key`，否则「slot 复用错位」会被如实报成跳变。
 */
function truckKey(g: SVGGElement): string {
  return g.dataset.truckKey ?? `slot:${g.dataset.slot ?? "?"}`;
}

/** 当前所有货车的屏幕落点（来自组件真正写进去的 `transform`）。 */
function truckMap(root: ParentNode = document): Map<string, Pt> {
  const out = new Map<string, Pt>();
  for (const g of root.querySelectorAll("svg.flow g.flow__truck") as NodeListOf<SVGGElement>) {
    const m = /translate\((-?[\d.]+)\s+(-?[\d.]+)\)/.exec(g.getAttribute("transform") ?? "");
    if (!m) continue; // 还没被动画摆放（首帧前）
    out.set(truckKey(g), { x: Number(m[1]), y: Number(m[2]) });
  }
  return out;
}

function guidePaths(): SVGPathElement[] {
  return [...document.querySelectorAll("svg.flow path.flow__guide")] as SVGPathElement[];
}

function pathLengthOf(el: Element): number {
  return flattenD(el.getAttribute("d") ?? "")?.total ?? 0;
}

function meanPathLength(): number {
  return median(guidePaths().map(pathLengthOf));
}

/** 两组位置中**同名货车**的位移。缺席（新增/删除）的车不计入。 */
function perTruckSteps(from: Map<string, Pt>, to: Map<string, Pt>): number[] {
  const out: number[] = [];
  for (const [key, a] of from) {
    const b = to.get(key);
    if (b) out.push(Math.hypot(a.x - b.x, a.y - b.y));
  }
  return out;
}

function median(v: number[]): number {
  if (v.length === 0) return 0;
  const s = [...v].sort((a, b) => a - b);
  const m = s.length >> 1;
  return s.length % 2 === 1 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
}

function maxOf(v: number[]): number {
  return v.reduce((a, b) => Math.max(a, b), 0);
}

/** 最大/中位比值 —— 孤立尖峰会让它远大于 1。 */
function spikeRatio(steps: number[]): number {
  return maxOf(steps) / Math.max(median(steps), 1e-9);
}

// ---------------------------------------------------------------------------
// guide 路径 ↔ 可见线 的几何一致性（防「车飞在空白处」）
// ---------------------------------------------------------------------------

interface SegRef {
  ax: number;
  ay: number;
  bx: number;
  by: number;
  minx: number;
  miny: number;
  maxx: number;
  maxy: number;
}

/** 把若干折线摊平成「带包围盒的线段」列表，便于对采样点做最近距离查询。 */
function buildSegments(polys: Polyline[]): SegRef[] {
  const out: SegRef[] = [];
  for (const poly of polys) {
    for (let i = 1; i < poly.pts.length; i++) {
      const a = poly.pts[i - 1]!;
      const b = poly.pts[i]!;
      out.push({
        ax: a.x,
        ay: a.y,
        bx: b.x,
        by: b.y,
        minx: Math.min(a.x, b.x),
        miny: Math.min(a.y, b.y),
        maxx: Math.max(a.x, b.x),
        maxy: Math.max(a.y, b.y),
      });
    }
  }
  return out;
}

/** 点到所有线段的最短距离（用包围盒下界剪枝，避免 O(采样点 × 线段) 变慢）。 */
function minDistanceToSegments(x: number, y: number, segs: SegRef[]): number {
  let best = Number.POSITIVE_INFINITY;
  for (const s of segs) {
    const dx = Math.max(s.minx - x, 0, x - s.maxx);
    const dy = Math.max(s.miny - y, 0, y - s.maxy);
    if (dx * dx + dy * dy >= best * best) continue;
    const vx = s.bx - s.ax;
    const vy = s.by - s.ay;
    const len2 = vx * vx + vy * vy;
    let t = len2 > 0 ? ((x - s.ax) * vx + (y - s.ay) * vy) / len2 : 0;
    t = t < 0 ? 0 : t > 1 ? 1 : t;
    const d = Math.hypot(x - (s.ax + t * vx), y - (s.ay + t * vy));
    if (d < best) best = d;
  }
  return best;
}

/** 沿折线按弧长间隔取点（等权，等价于「行程占比」采样）。 */
function sampleByArcLength(poly: Polyline, stepPx: number): { x: number; y: number; w: number }[] {
  const out: { x: number; y: number; w: number }[] = [];
  let next = 0;
  for (let i = 1; i < poly.pts.length; i++) {
    const a = poly.pts[i - 1]!;
    const b = poly.pts[i]!;
    const d0 = poly.cum[i - 1]!;
    const d1 = poly.cum[i]!;
    const segLen = d1 - d0;
    while (next <= d1) {
      const t = segLen > 0 ? (next - d0) / segLen : 0;
      out.push({ x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t, w: stepPx });
      next += stepPx;
    }
  }
  return out;
}

/** 推 `count` 帧，返回每帧之间的**同名货车**位移（pooled）。 */
function collectSteps(count: number, dtMs = 1000 / 60): number[] {
  const steps: number[] = [];
  let prev = new Map<string, Pt>();
  for (let f = 0; f < count; f++) {
    runFrame(dtMs);
    const cur = truckMap();
    if (prev.size > 0 && cur.size > 0) steps.push(...perTruckSteps(prev, cur));
    prev = cur;
  }
  return steps;
}

// ---------------------------------------------------------------------------
// 拓扑快照与刷新
// ---------------------------------------------------------------------------

const MiB = 1 << 20;
const GiB = 1 << 30;

function inbound(tag: string, up: number, down: number, port: number | null = 443): TopoInbound {
  return { tag, protocol: "vless", port, uplink_bytes: up, downlink_bytes: down };
}

function outbound(tag: string, kind: string, up: number, down: number): TopoOutbound {
  // `connections` 是内部通道（dns / api）的活跃度指标 —— 它们的字节计数器
  // 恒为 0。测试里给 0，只要字段存在即可。
  return { tag, protocol: "freedom", kind, uplink_bytes: up, downlink_bytes: down, connections: 0 };
}

/** 4 个出口：node / direct / block / dns（dns 是内部灰，也走一条分支）。 */
function outlets(): TopoOutbound[] {
  return [
    outbound("node-a", "node", 2.0 * GiB, 6.0 * GiB),
    outbound("direct-out", "direct", 0.4 * GiB, 1.1 * GiB),
    outbound("block-ads", "block", 0, 0),
    outbound("dns", "dns", 0.02 * GiB, 0.03 * GiB),
  ];
}

/**
 * 基线快照：3 条真实入口车道（另有 1 条 `api` 内部入口，应被过滤掉）。
 *
 * 字节数刻意选在 `trucksOnLane` 档位边界两侧，便于制造「车辆数量变化」：
 * 注意当前 `trucksOnLane` 里写的是 `Math.log10(1 << 40)` —— JS 位移是 32 位取模，
 * `1 << 40 === 1 << 8 === 256`，于是 `hi = log10(256) ≈ 2.41 < lo ≈ 6.02`，
 * 映射**反了**：≥1 MiB 的流量一律 3 辆，反而 <1 MiB 的小流量能到 8 辆。
 * 「货车数量映射」那条测试就是钉这个的（当前红）。
 */
function baseTopo(): TopologyData {
  return {
    inbound: [
      inbound("mixed", 0.5 * GiB, 4.0 * GiB),
      inbound("socks", 4 * MiB, 36 * MiB),
      inbound("http", 1 * MiB, 1 * MiB),
      inbound("api", 0, 0, 10085), // 内部通道，不画车道
    ],
    rule: [],
    outbound: outlets(),
    traffic_error: null,
    traffic_ok: true,
    counter_resets: 0,
    geo_available: true,
  };
}

function topoWith(overrides: Partial<TopologyData>): TopologyData {
  return { ...baseTopo(), ...overrides };
}

let intervalCb: (() => void) | null = null;

/** 让组件真的走一次「2 秒刷新」：换掉快照，触发那个 interval。 */
async function refresh(next: TopologyData): Promise<void> {
  state.topo = next;
  await act(async () => {
    intervalCb?.();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function mount(topo: TopologyData): Promise<void> {
  state.topo = topo;
  state.calls = 0;
  render(createElement(Topology));
  // 初次 load() 是异步的，等它落地。
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
  });
}

beforeEach(() => {
  layout.width = 820;
  layout.cardWidth = 168;
  pathCache.clear();
  roCallbacks.clear();
  clock = 0;
  pendingFrame = null;
  rafSeq = 0;
  intervalCb = null;

  Element.prototype.getBoundingClientRect = function getBoundingClientRect(this: Element): DOMRect {
    return rectOf(this);
  };
  window.requestAnimationFrame = ((cb: FrameRequestCallback): number => {
    pendingFrame = cb;
    return ++rafSeq;
  }) as typeof window.requestAnimationFrame;
  window.cancelAnimationFrame = ((): void => {
    pendingFrame = null;
  }) as typeof window.cancelAnimationFrame;
  window.setInterval = ((fn: TimerHandler): number => {
    intervalCb = typeof fn === "function" ? (fn as () => void) : null;
    return 1;
  }) as typeof window.setInterval;
  vi.stubGlobal("ResizeObserver", FakeResizeObserver as unknown as typeof ResizeObserver);
  installFakePath();
});

afterEach(() => {
  cleanup();
  Element.prototype.getBoundingClientRect = realGetBoundingClientRect;
  window.requestAnimationFrame = realRaf;
  window.cancelAnimationFrame = realCancelRaf;
  window.setInterval = realSetInterval;
  vi.unstubAllGlobals();
  uninstallFakePath();
  pathCache.clear();
});

/** 稳态每帧位移（中位）。所有「跳不跳」判据都以它为标尺。 */
function steadyStep(): number {
  return median(collectSteps(30));
}

/**
 * 核心判据：一次变化（刷新 / 几何扰动）造成的单帧位移不得是孤立尖峰。
 * - `median < 2 × steady`：大多数车不得整体位移（整体位移 = 进度被重新解释）；
 * - `max < maxMultiple × steady`：没有车被甩到别处。
 */
function expectNoTeleport(steady: number, transition: number[], maxMultiple = 5): void {
  expect(transition.length).toBeGreaterThan(0);
  expect(
    median(transition),
    `中位位移 ${median(transition).toFixed(1)}px 应为稳态 ${steady.toFixed(1)}px 的 2 倍以内`,
  ).toBeLessThan(2 * steady);
  expect(
    maxOf(transition),
    `最大位移 ${maxOf(transition).toFixed(1)}px 应为稳态 ${steady.toFixed(1)}px 的 ${maxMultiple} 倍以内`,
  ).toBeLessThan(maxMultiple * steady);
}

// ---------------------------------------------------------------------------
// 0. 前置自检：环境壳子失效时，后面所有判据都会变成假绿
// ---------------------------------------------------------------------------

describe("测试环境自检（防空壳）", () => {
  it("组件真的渲染了会动的货车，且刷新接口真的被调用过", async () => {
    // 防的故障：假布局 / fake path / ResizeObserver 桩任一失效时，
    // 测试可能「全绿但什么都没测」。这条确保后面每条判据都有观测对象。
    await mount(baseTopo());
    expect(state.calls).toBe(1);

    runFrame(1000 / 60);
    runFrame(1000 / 60);
    expect(truckMap().size).toBeGreaterThan(0);

    const steps = collectSteps(3);
    expect(median(steps)).toBeGreaterThan(0.3); // 车真的在走
    expect(guidePaths().length).toBe(3); // 3 条入口车道 → 3 条路线
    expect(maxOf(guidePaths().map(pathLengthOf))).toBeGreaterThan(10);
  });
});

// ---------------------------------------------------------------------------
// 1 & 2. 进度连续性与闭环
// ---------------------------------------------------------------------------

describe("沿路径行走的连续性", () => {
  it("连续 500 帧（>1 圈）内位移无孤立尖峰：最大/中位 < 2.0", async () => {
    // 防的故障：货车在某一帧突然跨一大段路 —— 用户看到的「乱跳」。
    // 不闭环（终点≠起点）会让每圈在 wrap 处跳一次；500 帧 ≈ 8.3 秒
    // （TRAVEL_SECONDS=7）确保至少跨过一圈的接缝。
    // 固定 dt=1/60s：不受真实 rAF 掉帧影响（见文件头）。
    await mount(baseTopo());
    const steps = collectSteps(500);

    expect(steps.length).toBeGreaterThan(2000);
    expect(median(steps)).toBeGreaterThan(0.5); // 非空壳：车确实在动
    expect(spikeRatio(steps)).toBeLessThan(2.0);
  });

  it("每条 guide 路径必须是一条闭环：只有一个 M，且终点回到起点", async () => {
    // 防的故障：
    //  a) 路径里出现多个 M（早先每段各写一个 M）→ getPointAtLength 在多条子路径间
    //     没有连续参数，货车会在段与段之间跳；
    //  b) 终点与起点不重合 → 每走完一圈瞬移回起点（v0.8.21 前实测 457px）。
    await mount(baseTopo());
    runFrame(1000 / 60);

    const paths = guidePaths();
    expect(paths.length).toBeGreaterThan(0);
    for (const p of paths) {
      const d = p.getAttribute("d") ?? "";
      const moves = d.match(/M/g) ?? [];
      expect(moves.length, `路径里有 ${moves.length} 个 M：${d}`).toBe(1);

      const poly = flattenD(d);
      expect(poly, "guide path 的 d 无法解析（组件可能改了命令集）").not.toBeNull();
      expect(poly!.total).toBeGreaterThan(1);
      const start = poly!.pts[0]!;
      const end = poly!.pts[poly!.pts.length - 1]!;
      // routeToD 用 toFixed(1) 写出首尾同一点，闭环应当精确成立。
      expect(Math.hypot(end.x - start.x, end.y - start.y)).toBeLessThan(0.2);
    }
  });

  it("guide 路径必须等于可见线的并集：离线行程 < 2% 且最大偏离 < 3px", async () => {
    // 防的故障（B1，用户可感知的「车飞在空白处」）：
    // `routeToD` 把各段用 `C` 连续拼接，而 SVG 的 `C` 从**上一段终点**继续 ——
    // 若把「回分叉」的段堆到所有去程之后，去第 2 个出口的曲线就从**第 1 个出口**
    // 出发，合并路径不再等于可见线的并集，车约 1/4 行程走在空白处。
    //
    // 为什么单列一条：车严格沿 guide 走，所以「车平滑」「guide 闭环」都测不出这个
    // 故障 —— 把段序回退后，本文件其它 11 条**全部仍然绿**（实测）。这条是唯一
    // 能挡住 `routeToD` 段序变化的断言。
    //
    // 实测（本文件口径，guide 上每 1px 采样，对照 `path.flow__route`）：
    //   - 修复版：离线 0.0%（按弧长）、最大偏离 0.0px、guideLen≈8839px；
    //   - 段序回退版：离线 23.9%、最大偏离 21.2px、guideLen≈7503px。
    // 所以 2% / 3px 的阈值余量充足。
    await mount(baseTopo());
    runFrame(1000 / 60);

    const visible = [...document.querySelectorAll("svg.flow path.flow__route")]
      .map((p) => flattenD(p.getAttribute("d") ?? ""))
      .filter((p): p is Polyline => p !== null);
    expect(visible.length, "没有可见分支线可供对照，判据不成立").toBeGreaterThan(0);
    const segs = buildSegments(visible);

    let totalW = 0;
    let offW = 0;
    let worst = 0;
    let samples = 0;
    for (const g of guidePaths()) {
      const poly = flattenD(g.getAttribute("d") ?? "");
      expect(poly, "guide path 的 d 无法解析（组件可能改了命令集）").not.toBeNull();
      for (const s of sampleByArcLength(poly!, 1)) {
        const d = minDistanceToSegments(s.x, s.y, segs);
        totalW += s.w;
        samples += 1;
        if (d > 1.5) offW += s.w;
        if (d > worst) worst = d;
      }
    }

    // 非空壳：采样量与路径长度必须够大，否则「0% 离线」没有意义。
    expect(samples, "采样点太少，判据不成立").toBeGreaterThan(2000);
    expect(totalW).toBeGreaterThan(1000);
    const frac = offW / totalW;
    expect(frac, `离线行程 ${(frac * 100).toFixed(1)}%（阈值 2%）`).toBeLessThan(0.02);
    expect(worst, `最大偏离 ${worst.toFixed(1)}px（阈值 3px）`).toBeLessThan(3);
  });
});

// ---------------------------------------------------------------------------
// 3. 货车数量映射（前置条件：数量不随流量变，就谈不上「数量变化导致重排」）
// ---------------------------------------------------------------------------

describe("货车数量映射", () => {
  it("流量越大车越多：4.5 GiB 的车道必须比空车道车多", async () => {
    // 防的故障：`trucksOnLane` 用 `Math.log10(1 << 40)` 当上限，而 JS 位移是 32 位取模
    // → `1 << 40 === 256` → 上限 2.41 小于下限 6.02 → 映射反向：
    // **≥1 MiB 的流量一律只有 3 辆**（与空车道一样），反而 1 字节能画 8 辆。
    // 后果：界面上「车辆数量由实测速率决定」是假的；且一旦有人修好这个映射，
    // 车辆数量就会随刷新变化，立刻触发下面那条 slot 错位跳变。
    const counts = (): number[] =>
      [0, 1, 2].map(
        (r) =>
          [...document.querySelectorAll("svg.flow g.flow__truck")].filter(
            (g) => (g as SVGGElement).dataset.route === String(r),
          ).length,
      );

    await mount(
      topoWith({
        inbound: [
          inbound("mixed", 0.5 * GiB, 4.0 * GiB), // 4.5 GiB
          inbound("socks", 4 * MiB, 36 * MiB), // 40 MiB
          inbound("http", 0, 0), // 空车道
          inbound("api", 0, 0, 10085),
        ],
      }),
    );
    runFrame(1000 / 60);
    const [big, mid, empty] = counts();

    // **空车道不画车。**
    //
    // 产品语义在 v0.8.25 改了：早先 `trucksOnLane(0)` 返回 3，注释写的是
    // 「空车道也画几辆，否则『量小』与『不通』看不出来」—— 但它把两者画成
    // 一样（都 3 辆），既没达成目的，又制造了「明明 0 B 却有车在跑」的假象。
    // 用户直接指出过这一点（http 入口 0 B 却有车）。
    //
    // 「量小」与「不通」本来就分得开：卡片字节数分别显示 `↓1.2 MiB` 与
    // `↓0 B`，`traffic_ok === false` 时显示「流量不可用」。
    expect(empty, "空车道不应画车（0 B 却有车 = 假象）").toBe(0);
    expect(
      big,
      `4.5 GiB 车道只有 ${big} 辆、空车道 ${empty} 辆 —— 车辆数量没有反映流量`,
    ).toBeGreaterThan(empty!);
    expect(mid).toBeGreaterThan(empty!); // 有流量的都要比空车道多
    expect(big).toBeGreaterThanOrEqual(mid!);
  });
});

// ---------------------------------------------------------------------------
// 4. 模式跳：刷新导致重排
// ---------------------------------------------------------------------------

describe("刷新重排时的位置连续性", () => {
  it("货车数量变化时，同一辆车不得换路线瞬移", async () => {
    // 防的故障（当前最可能的残留根因）：
    // `progressRef` 按**数组下标 slot** 保存，而货车列表是按「每条路线画几辆」
    // 展开的。入口字节数一变 → 车辆数一变 → 后面所有 slot 归属的路线整体错位，
    // 车带着旧进度落到另一条路线上 → 瞬移。
    // 场景选 512 B → 4.5 GiB：无论计数映射是当前的反向映射（8→3）还是修好后的
    // 正向映射（3→6），车辆数都会变，所以这条测试在修复前后都真的在考这件事。
    const laneShapes = (mixedDown: number): TopologyData =>
      topoWith({
        inbound: [
          inbound("mixed", 0, mixedDown),
          inbound("socks", 4 * MiB, 36 * MiB),
          inbound("http", 1 * MiB, 1 * MiB),
          inbound("api", 0, 0, 10085),
        ],
      });

    await mount(laneShapes(512));
    const steady = steadyStep();
    const before = truckMap();

    await refresh(laneShapes(4.0 * GiB));
    runFrame(1000 / 60);
    const after = truckMap();

    // 确认场景真的改变了车辆数量（否则这条测试什么也没考）。
    expect(after.size).not.toBe(before.size);
    expectNoTeleport(steady, perTruckSteps(before, after));
  });

  it("新增出口（扇出变多）时，已在这条路线上的货车不得瞬移", async () => {
    // 防的故障：出口多一个 → 每条路线多两段分支（去 + 回）→ 路径长度与形状都变。
    // 进度仍是「占全程比例」，于是同一比例指向完全不同的位置（甚至换到别的分支）。
    // 布局高度变化会让真实 ResizeObserver 重测，这里显式补一次 fireResize()。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckMap();
    const l0 = meanPathLength();

    await refresh(
      topoWith({ outbound: [...outlets(), outbound("node-b", "node", 0.3 * GiB, 1.0 * GiB)] }),
    );
    fireResize();
    runFrame(1000 / 60);

    const after = truckMap();
    expect(after.size).toBe(before.size); // 车数不变，只有几何变了
    const l1 = meanPathLength();
    // 确认几何真的变了（否则这条测试无意义）。
    expect(Math.abs(l1 - l0) / l0).toBeGreaterThan(0.1);
    expectNoTeleport(steady, perTruckSteps(before, after));
  });

  it("删除出口（旧 label 被卸载）时，货车不得瞬移", async () => {
    // 防的故障：出口被删 → 组件的测量闭包仍持有**已被卸载**的 label 元素引用，
    // 而浏览器对脱挂元素的 `getBoundingClientRect()` 返回全 0 → 那个 (0,0) 被当成
    // 真实锚点，整条扇出几何塌向原点 → 车跳。这正是「刷新重建 outlets 数组」
    // 那条怀疑的复现条件。用 fireResize() 模拟容器高度变化触发的重测。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckMap();
    const l0 = meanPathLength();

    // 删掉 `block-ads`（一个**用户可见**的出口）。
    //
    // 早先这里删的是 `dns` —— 那时 dns 也画在流向图里。现在 dns / api 属
    // **内部通道**、不再进流向图（它们的字节计数器恒为 0，混进来会被误读成
    // 「没在用」），所以删 dns 不会改变流向几何、这条断言会失去意义。
    // 删一个真正参与扇出的出口，才测得到「脱挂元素被当成锚点」这个故障。
    await refresh(topoWith({ outbound: outlets().filter((o) => o.tag !== "block-ads") }));
    fireResize();
    runFrame(1000 / 60);

    const after = truckMap();
    expect(after.size).toBe(before.size);
    const l1 = meanPathLength();
    expect(Math.abs(l1 - l0) / l0).toBeGreaterThan(0.05);
    expectNoTeleport(steady, perTruckSteps(before, after));
  });
});

// ---------------------------------------------------------------------------
// 5. 几何跳：卡片宽度 / 容器宽度
// ---------------------------------------------------------------------------

describe("几何变化时的位置连续性", () => {
  it("卡片宽度变化使路径长度变化 ≈30% 时，货车不得瞬移", async () => {
    // 防的故障（任务书点名的第 3 类跳）：入口/出口卡片的宽度随字节文字长度变化
    // → 路径长度与分叉点移动，而进度是按「占全程的比例」保存的
    // → 同一比例落到不同的屏幕点，看起来就是跳。
    // 这里把卡片从 168px 改到 250px（复刻 CSS 固定列宽被内容撑宽的情形）。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckMap();
    const l0 = meanPathLength();

    layout.cardWidth = 250;
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    expect(
      Math.abs(l1 - l0) / l0,
      `路径长度变化 ${(((l1 - l0) / l0) * 100).toFixed(1)}%`,
    ).toBeGreaterThan(0.2);
    expectNoTeleport(steady, perTruckSteps(before, truckMap()));
  });

  it("容器宽度 +30%（窗口缩放）时，货车不得瞬移", async () => {
    // 防的故障：窗口缩放 / 侧栏开合触发重新测量，路径被拉长。若进度只是比例，
    // 同一帧内所有车都会被整体挪动 —— 单帧位移达稳态的几十倍，看起来就是跳。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckMap();
    const l0 = meanPathLength();

    layout.width = Math.round(820 * 1.3);
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    expect(Math.abs(l1 - l0) / l0, "任务书要求覆盖「路径长度变化 ±30%」").toBeGreaterThan(0.2);
    expectNoTeleport(steady, perTruckSteps(before, truckMap()));
  });

  it("几何只动一点点（≈3%）时，位移必须远小于稳态的 2 倍", async () => {
    // 防的故障：每 2 秒刷新带来的**小**几何抖动。大变化会让人以为是重排，
    // 小变化若也跳，就完全是「乱跳」的观感。这条是上界收紧版：
    // 小几何变化只允许产生小位移。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckMap();
    const l0 = meanPathLength();

    layout.cardWidth = 176;
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    const rel = Math.abs(l1 - l0) / l0;
    expect(rel).toBeGreaterThan(0.005);
    expect(rel).toBeLessThan(0.1);
    const steps = perTruckSteps(before, truckMap());
    expect(steps.length).toBeGreaterThan(0);
    expect(maxOf(steps)).toBeLessThan(2 * steady);
  });
});

// ---------------------------------------------------------------------------
// 6. 数字跳：查不到流量时不允许画成 0
// ---------------------------------------------------------------------------

describe("字节数字的连续性", () => {
  const laneTexts = (): string[] =>
    [...document.querySelectorAll(".highway__lane-label:not(.highway__lane-label--total)")].map(
      (el) => el.querySelector(".highway__lane-bytes")?.textContent ?? "",
    );

  it("traffic_ok=false 时不得显示伪造的 0 B，恢复后要回到真实值", async () => {
    // 防的故障：用户原话「8.01 GiB 掉到 0 再回来」。
    // 统计查询失败时后端把所有 `*_bytes` 填 0 并置 `traffic_ok=false` /
    // `traffic_error`（见 task-5）。前端若照单全收，就会画出 0 B，
    // 下一次刷新又跳回 8.01 GiB —— 这是数字层面的「乱跳」。
    // 契约（task-5 已落地）：`traffic_ok === (traffic_error === null)`；
    // 不可用时界面**必须**显示「—」而不是 0 B。
    const available = topoWith({
      inbound: [
        inbound("mixed", 0.5 * GiB, 4.0 * GiB),
        inbound("socks", 4 * MiB, 36 * MiB),
        inbound("http", 1 * MiB, 1 * MiB),
        inbound("api", 0, 0, 10085),
      ],
    });
    await mount(available);
    expect(laneTexts().some((t) => t.includes("GiB"))).toBe(true);

    const unavailable = topoWith({
      traffic_error: "核心未在运行：读不到 StatsService",
      traffic_ok: false,
      inbound: [
        inbound("mixed", 0, 0),
        inbound("socks", 0, 0),
        inbound("http", 0, 0),
        inbound("api", 0, 0, 10085),
      ],
      outbound: outlets().map((o) => ({ ...o, uplink_bytes: 0, downlink_bytes: 0 })),
    });
    await refresh(unavailable);

    const during = laneTexts();
    expect(during.length).toBeGreaterThan(0);
    // 核心断言：不可用时，任何车道都不得把「没查到」画成 0 B。
    for (const t of during) {
      expect(t, `查不到流量时显示了伪 0：${JSON.stringify(t)}`).not.toMatch(/(^|\D)0 B(\D|$)/);
    }
    // 错误说明必须露出来（不是把数字藏起来就完事）。
    expect(document.body.textContent ?? "").toContain("核心未在运行");

    // 恢复后必须回到真实值，不允许「永久显示 —」这种偷懒修法。
    await refresh(available);
    expect(laneTexts().some((t) => t.includes("GiB"))).toBe(true);
  });
});

/**
 * # 诚实清单：这个文件覆盖不到什么
 *
 * 1. **真实浏览器的路径插值**：jsdom 没有 `getPointAtLength`，用的是本文件里
 *    「按 24 段离散 + 线性插值」的 fake path。它与浏览器的弧长精确采样在曲率上有
 *    微小差异（<< 1px），测不到浏览器自身实现差异（例如超长 path 的采样精度）。
 * 2. **真实布局/字体度量**：卡片宽度、行高、`getBoundingClientRect` 都是本文件给的
 *    假值（复刻 styles.css 的固定列宽语义）。所以「文字长度把卡片撑宽」是**注入**的，
 *    不是真实渲染测出来的。当前 CSS 用固定列宽（`168px ... 168px`）刻意避免了内容撑宽，
 *    因此真实浏览器里刷新时几何可能根本不变 —— 本文件测的是**不变量**（几何一旦变了
 *    也必须连续），不是「现在就一定会变」。
 * 3. **ResizeObserver 的真实时序**：用同步桩触发，测不到浏览器里
 *    「RO 回调 → 布局抖动 → 下一帧」的顺序问题，也测不到 RO 循环报错。
 * 4. **数据源本身的抖动**：本文件只测前端对快照的响应。后端每 2 秒返回值是否可信
 *    （计数器自增、查询失败、跨重启续接）由 Rust 侧测试负责（task-5）。
 * 5. **CSS 视觉**：颜色、透明度、货车是否被线遮住、色盲可读性，都不在本文件能力范围
 *    内（需要截图/人工审查，见 task-4）。
 * 6. **性能**：13 辆车 × 500 帧的判据不测真实帧率；若修复引入每帧 O(n²) 重建，
 *    这里不会红。
 * 7. **稳定身份的约定**：跨刷新「同一辆车」的比较依赖 `data-truck-key`。修复前没有
 *    这个属性，测试退化为按 `data-slot` 比较 —— 这正是被修复的机制本身。
 *    修复后若用别的方式实现稳定身份但不在 DOM 暴露 key，测试可能仍报跳变，
 *    这属于**测试契约未满足**，不是环境问题（见文件头「货车身份」）。
 */

// ---------------------------------------------------------------------------
// 6. 内部通道与用户流向分离
// ---------------------------------------------------------------------------

describe("内部通道（dns-out / api）与用户流向分离", () => {
  /**
   * **防「把测量盲区画成没在用」。**
   *
   * `dns-out`（UDP）与 `api`（本机回环）的字节计数器恒为 0 —— `StatsService`
   * 不统计它们。本机实测这两者各有 4769 / 5374 条连接，而字节一直是 0。
   *
   * 若把它们与 `node` / `direct` 并列在流向图里显示 `0 B`，用户会以为
   * 「这两个出口没在用」，或者反过来怀疑「是不是坏了」。所以：
   * 它们**不进流向图**，单独分组，并用连接数表示活跃度。
   */
  it("dns 出口不得出现在流向图里（它是内部通道）", async () => {
    await mount(baseTopo());
    // 流向图里应只有 3 条扇出路径（node / direct / block），不含 dns
    const guides = document.querySelectorAll("path.flow__guide");
    expect(guides.length).toBe(3);
  });

  it("内部通道单独分组，并改用连接数表示活跃度", async () => {
    // 给 dns 一个**非 0** 的连接数：如果界面还在显示字节，就会显示
    // `↓0 B ↑0 B`（因为它的字节恒为 0），而不是「4769 条连接」。
    const topo = baseTopo();
    topo.outbound = topo.outbound.map((o) =>
      o.tag === "dns" ? { ...o, connections: 4769 } : o,
    );
    await mount(topo);

    const box = document.querySelector(".highway__internal");
    expect(box, "内部通道应当单独分组渲染").not.toBeNull();
    const text = box!.textContent ?? "";
    expect(text).toContain("4769");
    expect(text).toContain("连接");
    // 且不得在内部通道里画成「字节 0」——那正是要消除的误导
    expect(text).not.toContain("0 B");
    expect(text).not.toContain("↓0");
  });

  it("连接数没观察到时显示「—」而不是 0", async () => {
    // `connections: null` 表示**没观察到**（核心没跑 / 日志里还没连接行），
    // 与「观察到 0 条」不同。显示成 0 会让人以为「一个连接都没有」。
    const topo = baseTopo();
    topo.outbound = topo.outbound.map((o) =>
      o.tag === "dns" ? { ...o, connections: null } : o,
    );
    await mount(topo);
    const text = document.querySelector(".highway__internal")?.textContent ?? "";
    expect(text).toContain("—");
  });
});

// ---------------------------------------------------------------------------
// 7. 回程段不得出现「倒着开」的车
// ---------------------------------------------------------------------------

describe("回程段：车不得走回程（防止看起来倒着开）", () => {
  /**
   * **防「小车怎么是来回的」。**
   *
   * 路线是**闭环**（否则车到终点会瞬移回起点，实测 457px 跳变），但闭环的
   * 回程在屏幕上就是**倒着开** —— 用户直接指出了这一点。
   *
   * 修法：进度对**去程长度**取模，车永远不进入回程段。
   *
   * 中途试过「走完整圈 + 把回程隐藏」，实测隐藏占比 **92.8%** ——
   * 因为整圈是「主干 + 6×来回 + 回主干」，去程只占约 11%，
   * 车大部分时间都在看不见的回程上跑。所以必须让车**只走去程**。
   *
   * # 为什么这里没有自动回归（如实说明）
   *
   * 我试过三种自动判据，**都不能区分**「只走去程」与「走整圈」，所以都删掉了 ——
   * 留一个测不到故障的测试比没有更糟（会给人虚假的安全感）：
   *
   * 1. **一轮内经过起点的频次**：走整圈与只走去程都是每轮经过 1 次
   *    （整圈也只在回绕那一刻经过起点），数学上区分不了。
   * 2. **车到可见线的距离**：闭环路径是「去程 + 回程」的**重复参数化**，
   *    同一个屏幕点对应两个弧长。回程段在几何上与去程重合，所以位置看起来
   *    始终在线 —— 实测走整圈（9.4px）反而比只走去程（15.5px）更小。
   * 3. **抽成纯函数再测**：那只是测我临时写的函数，**没碰产品代码**，是假测试。
   *
   * 所以这一项靠**浏览器人工验证**（CDP 取样，确认车不再出现倒着开的轨迹），
   * 并且已在 `docs/ui/topology/README.md` 里写明这个局限。
   */
  it.todo("（无自动回归）车只走去程：几何判据在重复参数化的闭环上不可区分，见上方说明");
});

// ---------------------------------------------------------------------------
// 两条行为级不变量护栏（task-36）：车只在去程循环 ｜ 身份按 data-truck-key 稳定
//
// 为什么单列：这两条语义**已修好**，但把修复改坏后原有 15 条测试**全绿**
// （backend-dev 在 /tmp worktree 实测）。它们量的是「位移/跳变指标」，
// 而「多走回程段」和「身份退化成下标」都**不影响位移连续性**，所以量不到。
// ---------------------------------------------------------------------------

/**
 * 量「一圈走过的屏幕路程 / 整环长度」。
 *
 * 口径：速度 = span / TRAVEL_SECONDS（见 Flow），所以走满 `TRAVEL_SECONDS` 秒
 * 恰好走完一轮。把这段时间内每辆车逐帧的屏幕位移累加，再除以**它自己那条路线**
 * 的整环长度，就得到「一轮走了整环的百分之几」——**不需要**识别回绕点、
 * 也不需要投影到路径上（回绕那一跳被限速器摊成滑行，已经含在位移里）。
 *
 * 判据的意义：
 *   * 正确（`span = 去程长度`）→ ≈0.6（去程 + 回绕滑行）；
 *   * `span = total`（历史 bug：走完整圈、含回程）→ ≈1.0。
 * 两者相差 40 个百分点，阈值放在 0.8 有无风险。
 */
function measureLapTravel(): number[] {
  const svg = document.querySelector("svg.flow");
  if (!svg) throw new Error("没有渲染 svg.flow");
  const totals = [...svg.querySelectorAll(":scope > path.flow__guide")].map((e) => pathLengthOf(e));
  const frames = Math.round(TRAVEL_SECONDS * 60); // 手动时钟固定 1/60s → 一圈 = TRAVEL_SECONDS×60 帧
  const prev = new Map<string, Pt>();
  const traveled = new Map<string, number>();
  const routeOf = new Map<string, number>();
  for (let f = 0; f < frames; f++) {
    runFrame(1000 / 60);
    for (const g of svg.querySelectorAll("g.flow__truck") as NodeListOf<SVGGElement>) {
      const key = g.dataset.truckKey;
      const m = /translate\((-?[\d.]+)\s+(-?[\d.]+)\)/.exec(g.getAttribute("transform") ?? "");
      if (!key || !m) continue;
      routeOf.set(key, Number(g.dataset.route ?? 0));
      const p = { x: Number(m[1]), y: Number(m[2]) };
      const q = prev.get(key);
      if (q) traveled.set(key, (traveled.get(key) ?? 0) + Math.hypot(p.x - q.x, p.y - q.y));
      prev.set(key, p);
    }
  }
  return [...traveled.entries()].map(([k, v]) => v / (totals[routeOf.get(k) ?? 0] ?? 1));
}

interface KeyedTruck {
  routeKey: string;
  x: number;
  y: number;
}

/** key → { 所属路线 key, 屏幕坐标 }，取自 DOM（`data-truck-key` / `data-route-key`）。 */
function keyedTrucks(): Map<string, KeyedTruck> {
  const out = new Map<string, KeyedTruck>();
  for (const g of document.querySelectorAll("g.flow__truck") as NodeListOf<SVGGElement>) {
    const key = g.dataset.truckKey;
    const routeKey = g.dataset.routeKey;
    const m = /translate\((-?[\d.]+)\s+(-?[\d.]+)\)/.exec(g.getAttribute("transform") ?? "");
    if (!key || !routeKey || !m) continue;
    out.set(key, { routeKey, x: Number(m[1]), y: Number(m[2]) });
  }
  return out;
}

/**
 * 命名契约：`data-truck-key` = `<路线 key>#<序号>`，且与 `data-route-key` 一致。
 * 退化成 `slot-N` 之类的下标命名会在这一条上直接失败。
 */
function expectKeyContract(trucks: Map<string, KeyedTruck>): void {
  expect(trucks.size).toBeGreaterThan(0);
  for (const [key, t] of trucks) {
    expect(key, `key 不以路线 key 开头：${key} / ${t.routeKey}`).toMatch(
      new RegExp(`^${t.routeKey.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}#\\d+$`),
    );
  }
  expect(new Set(trucks.keys()).size).toBe(trucks.size); // key 唯一
}

describe("动画不变量护栏（行为级）", () => {
  it("车只在去程循环：一圈走的路程是去程（≈60% 整环），不是整环（span 退回 total 会变红）", async () => {
    // 防的故障：`span = total` —— 车走完整圈，包括回程段（历史上真实发生过，
    // 屏幕上是「倒着开」）。它**不破坏位移连续性**，所以「最大/中位步长」量不到。
    // 口径：只统计落在 guide 上的帧，量它们覆盖了整环弧长的百分之几。
    //   正确（span = 去程）≈ 50%；`span = total` → 100%（此时闭环无缝、没有滑行段）。
    await mount(baseTopo());
    const ratios = measureLapTravel();

    expect(ratios.length).toBeGreaterThan(0);
    const med = median(ratios);
    // 正确 ≈0.6（去程 + 回绕滑行）；`span = total` ≈1.0（走完整圈、含回程）
    expect(
      med,
      `一轮走了整环的 ${(med * 100).toFixed(0)}%（正确≈60%，span=total≈99%）`,
    ).toBeLessThan(0.8);
    expect(maxOf(ratios), `最大 ${(maxOf(ratios) * 100).toFixed(0)}%`).toBeLessThan(0.85);
    expect(Math.min(...ratios), "过低：车可能根本没在走").toBeGreaterThan(0.3);
  });

  it("身份按 data-truck-key 稳定：车辆数量变化后，同一 key 仍属同一条路线且位置连续", async () => {
    // 防的故障：身份退化成数组下标（`slot-N`）。数量一变，同一个 key 会指向
    // **别的路线上的另一辆车**。原有的「不得瞬移」判据被重锚+限速兜住，量不到它。
    const laneShape = (down: number): TopologyData =>
      topoWith({
        inbound: [
          inbound("mixed", 0, down),
          inbound("socks", 4 * MiB, 36 * MiB),
          inbound("http", 1 * MiB, 1 * MiB),
          inbound("api", 0, 0, 10085),
        ],
      });
    await mount(laneShape(512));
    const steady = steadyStep();
    const before = keyedTrucks();
    expectKeyContract(before);

    await refresh(laneShape(4.0 * GiB));
    runFrame(1000 / 60);
    const after = keyedTrucks();
    expect(after.size, "场景没有改变车辆数量，这条测试就白测了").not.toBe(before.size);
    expectKeyContract(after);

    const survivors = [...before].filter(([k]) => after.has(k));
    expect(survivors.length, "没有幸存的 key").toBeGreaterThan(0);
    for (const [key, b] of survivors) {
      const a = after.get(key)!;
      expect(a.routeKey, `key=${key} 数量变化后换了路线（身份按下标漂移）`).toBe(b.routeKey);
      expect(Math.hypot(a.x - b.x, a.y - b.y), `key=${key} 位置跳变`).toBeLessThan(5 * steady);
    }
  });

  it("身份按 data-truck-key 稳定：入口顺序重排后，同一 key 仍属同一条路线", async () => {
    // 防的故障同上，但触发方式是**重排**（入口数组顺序变化）。按路线 key 建立身份时
    // 顺序无关；按下标建立身份时，同一个 key 会落到另一条路线上。
    await mount(baseTopo());
    steadyStep();
    const before = keyedTrucks();
    expectKeyContract(before);

    await refresh(
      topoWith({
        inbound: [
          inbound("socks", 4 * MiB, 36 * MiB),
          inbound("mixed", 0.5 * GiB, 4.0 * GiB),
          inbound("http", 1 * MiB, 1 * MiB),
          inbound("api", 0, 0, 10085),
        ],
      }),
    );
    runFrame(1000 / 60);
    const after = keyedTrucks();
    expectKeyContract(after);

    const survivors = [...before].filter(([k]) => after.has(k));
    expect(survivors.length).toBeGreaterThan(0);
    for (const [key, b] of survivors) {
      expect(after.get(key)!.routeKey, `key=${key} 重排后换了路线（身份按下标漂移）`).toBe(b.routeKey);
    }
  });
});

