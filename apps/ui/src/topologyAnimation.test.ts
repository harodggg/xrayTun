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

// ---------------------------------------------------------------------------
// 回绕 seam（task-51）：一种**设计好的**单帧大位移，必须从「不得瞬移」里豁免
// ---------------------------------------------------------------------------

/**
 * 识别「一圈的接缝」的判据（沿用 tester 在 `docs/ui/topology/DRIFT-EVIDENCE.md` 的口径）：
 * 同一辆车相邻帧 `data-arc` **骤降 > 300px**，**并且**车落回了一趟的**入口**
 * （`data-arc < 30px`）。两个条件是分开的实测结论：
 *
 * · **幅度**：真实页面上一圈弧长 ≈484–505px（48 次回绕无一漏判）。
 *   实测各种几何事件的弧长回退幅度：卡片宽 168→250 **82px**、容器 +30% **14.5px**、
 *   增/删出口 **1.5px** —— 都远在 300px 以下（本文件「豁免判据只认回到入口」那条钉住）。
 * · **落点**：回绕把 `st.dist` 归一到 `[0, walk)` 再当弧长，而 `walk = lap/420`
 *   （`TRAVEL_SECONDS=7`、60fps），本几何下 ≈1.1px；实测落点 **0.76–0.83px**。
 *   30px 给了 25 倍余量。加这条是为了防「大幅几何重锚恰好 >300px」被误豁免 ——
 *   那种情况落点在**半路**（实测 52.8 / 151.8 / 294.8），不是入口。
 *   代价：若将来一圈的每帧步长超过 30px（超长路线 / 超大 dt），这条会漏判回绕 ——
 *   那时尖峰会留在连续性判据里**变红**（响亮），而不是被静默放过（危险）。
 *
 * # 为什么要豁免
 *
 * 回绕那一帧车从分支末端**直接出现在**主干起点（task-51 的修法：瞬时落位），
 * 语义是「上一趟送达了」。它是**预期行为**，但会以 ~lap 的单帧位移出现在「不得瞬移」
 * 的判据里 —— 不豁免就是实现与测试互相矛盾。改前它是被限速器摊成 ~2.7s 的屏幕直线
 * 滑行，所以那时不需要豁免；现在需要。
 *
 * # 豁免窄到什么程度（由「渲染位置必须落在它宣称的弧长位置上」那组测试钉住）
 *
 * · 只跳过**这一辆车、这一帧**（按 key 匹配，其余车照旧量）；
 * · 被跳过的位移必须**真的落在 guide 上**，且落点在一趟的入口（设计的瞬时落位）；
 * · **几何变化**（resize / 出口增减）产生的重锚位移**不得**被跳过，那些仍要限速滑行。
 */
const SEAM_ARC_DROP = 300;
/** 回绕一定把车放回一趟的**入口**；几何重锚会把它放在半路。见上面的实测数字。 */
const SEAM_MAX_ARC = 30;

/** 一帧的「车 → (屏幕位置, data-arc)」。回绕判据需要相邻两帧的 `data-arc`。 */
interface TruckSnap {
  pos: Map<string, Pt>;
  arc: Map<string, number>;
}

function truckSnap(root: ParentNode = document): TruckSnap {
  const pos = new Map<string, Pt>();
  const arc = new Map<string, number>();
  for (const g of root.querySelectorAll("svg.flow g.flow__truck") as NodeListOf<SVGGElement>) {
    const m = /translate\((-?[\d.]+)\s+(-?[\d.]+)\)/.exec(g.getAttribute("transform") ?? "");
    if (!m) continue;
    const key = truckKey(g);
    pos.set(key, { x: Number(m[1]), y: Number(m[2]) });
    const a = Number(g.dataset.arc);
    if (Number.isFinite(a)) arc.set(key, a);
  }
  return { pos, arc };
}

/**
 * 相邻两帧之间「回绕（= 一趟送到了）」的车。只含这些 key。
 * 判据见 `SEAM_ARC_DROP` / `SEAM_MAX_ARC`：**弧长骤降 + 落回入口**，缺一不可。
 */
function seamKeys(from: TruckSnap, to: TruckSnap): Set<string> {
  const out = new Set<string>();
  for (const [key, before] of from.arc) {
    const after = to.arc.get(key);
    if (after === undefined) continue;
    if (before - after > SEAM_ARC_DROP && after < SEAM_MAX_ARC) out.add(key);
  }
  return out;
}

/**
 * 两组快照中**同名货车**的位移。默认跳过回绕 seam（见 `SEAM_ARC_DROP`）。
 * `includeSeam: true` 给出**未豁免**的原始序列 —— 用来证明豁免确实必要（不是空壳）。
 */
function stepsBetween(
  from: TruckSnap,
  to: TruckSnap,
  opts: { includeSeam?: boolean } = {},
): number[] {
  const seam = opts.includeSeam ? new Set<string>() : seamKeys(from, to);
  const out: number[] = [];
  for (const [key, a] of from.pos) {
    if (seam.has(key)) continue;
    const b = to.pos.get(key);
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

/**
 * 推 `count` 帧，返回每帧之间的**同名货车**位移（pooled）。
 * 默认**跳过回绕 seam**（task-51）：那一帧是设计的瞬时落位，见 `SEAM_ARC_DROP`。
 * `includeSeam: true` 给出未豁免的原始序列。
 */
function collectSteps(
  count: number,
  dtMs = 1000 / 60,
  opts: { includeSeam?: boolean } = {},
): number[] {
  const steps: number[] = [];
  let prev: TruckSnap | null = null;
  for (let f = 0; f < count; f++) {
    runFrame(dtMs);
    const cur = truckSnap();
    if (prev && prev.pos.size > 0 && cur.pos.size > 0) steps.push(...stepsBetween(prev, cur, opts));
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
// 1c. 渲染位置 = 它宣称的弧长位置（task-51 补的护栏：唯一能量到「车漂到线外」的判据）
// ---------------------------------------------------------------------------

/**
 * # 为什么必须补这条（原有护栏的**盲区**，task-49 实测量化）
 *
 * `data-arc` 是**意图**；用户看到的是 `transform`。改前的限速器在**屏幕空间**做线性
 * 插值，于是回绕后每圈有 ~2.7s 车沿一条直线飞回入口：`data-arc` 一路正确、
 * `transform` 一路离线。实测（`docs/ui/topology/DRIFT-EVIDENCE.md`，25s / 1,493 帧）：
 *
 * · |渲染 − P(data-arc)| > 1.5px 的帧-车占 **39.26%**；
 * · 其中 **6,216 帧（37.8%）的意图点仍精确在线上（≤0.5px）**；
 * · 滑行帧 48.14% 离线 vs 正常行走帧 3.11%；一圈里约 40% 的时间在滑行；
 * · 按通道：node（蓝）**33.85px** > direct 18.21 > block 9.67 —— 与用户「蓝车漂移」一致。
 *
 * 也就是说：**原有全部护栏（连续性 / 弧长区间 / 身份）在这些帧上都是绿的**，
 * 因为它们量的是 `data-arc`。这条量的是**结果**。
 *
 * # 与回绕豁免的关系
 *
 * 这条**不需要豁免**：瞬时落位之后，回绕那一帧的渲染点就是 P(arc)（偏差 0），
 * 所以「每一帧都要落在 P(data-arc) 上」在修好之后是**无条件成立**的。
 * 反过来说，把回绕改回限速滑行，这条会立刻变红 —— 见同组最后一条。
 */
describe("渲染位置必须落在它宣称的弧长位置上（task-51）", () => {
  /** `data-route-key` → 该路线的 guide 路径（DOM 顺序与 `geo.routes` 一致）。 */
  function guideByRouteKey(): Map<string, SVGPathElement> {
    const svg = document.querySelector("svg.flow");
    if (!svg) throw new Error("没有渲染 svg.flow");
    const keys = [...svg.querySelectorAll("g[data-route-paths]")].map(
      (e) => (e as SVGGElement).dataset.routePaths ?? "",
    );
    const guides = [...svg.querySelectorAll(":scope > path.flow__guide")] as SVGPathElement[];
    const out = new Map<string, SVGPathElement>();
    keys.forEach((k, i) => {
      const g = guides[i];
      if (g) out.set(k, g);
    });
    return out;
  }

  interface RenderGap {
    key: string;
    arc: number;
    gap: number;
  }

  /** 当前这一帧、每辆车的「渲染点 vs P(data-arc)」偏差（用页面里真实的 getPointAtLength）。 */
  function gapsOfFrame(guides: Map<string, SVGPathElement>): RenderGap[] {
    const out: RenderGap[] = [];
    for (const g of document.querySelectorAll(
      "svg.flow g.flow__truck",
    ) as NodeListOf<SVGGElement>) {
      const path = guides.get(g.dataset.routeKey ?? "");
      const m = /translate\((-?[\d.]+)\s+(-?[\d.]+)\)/.exec(g.getAttribute("transform") ?? "");
      const arc = Number(g.dataset.arc);
      if (!path || !m || !Number.isFinite(arc)) continue;
      const expected = (
        path as SVGPathElement & { getPointAtLength(l: number): Pt }
      ).getPointAtLength(arc);
      out.push({
        key: truckKey(g),
        arc,
        gap: Math.hypot(Number(m[1]) - expected.x, Number(m[2]) - expected.y),
      });
    }
    return out;
  }

  /** 推 `frames` 帧，统计渲染位置偏差；同时数出窗口里真实的回绕次数（防「窗口里没有接缝」的假绿）。 */
  function scanRenderGap(frames: number): {
    samples: number;
    over: number;
    max: number;
    worstKey: string;
    wraps: number;
  } {
    const guides = guideByRouteKey();
    let samples = 0;
    let over = 0;
    let max = 0;
    let worstKey = "";
    let wraps = 0;
    let prev = truckSnap();
    for (let f = 0; f < frames; f++) {
      runFrame(1000 / 60);
      const cur = truckSnap();
      wraps += seamKeys(prev, cur).size;
      prev = cur;
      for (const r of gapsOfFrame(guides)) {
        samples += 1;
        if (r.gap > 1.5) over += 1;
        if (r.gap > max) {
          max = r.gap;
          worstKey = r.key;
        }
      }
    }
    return { samples, over, max, worstKey, wraps };
  }

  it("每一帧每一辆车：渲染点必须落在 P(data-arc) 上（≤1.5px）", async () => {    await mount(baseTopo());
    const s = scanRenderGap(500); // 500 帧 ≈ 8.3s，跨过 ≥1 次回绕

    // 非空壳：采样量必须够大，且窗口里**真的发生过回绕**（否则这条对回绕不敏感）。
    expect(s.samples, "采样太少，判据不成立").toBeGreaterThan(2000);
    expect(s.wraps, "窗口里没有一次回绕，这条对回绕不敏感").toBeGreaterThan(0);

    expect(
      s.max,
      `最大偏差 ${s.max.toFixed(3)}px（阈值 1.5px，最差车 ${s.worstKey}）`,
    ).toBeLessThan(1.5);
    expect(s.over, `有 ${s.over} 个帧-车偏差 > 1.5px`).toBe(0);
  });

  /**
   * 逐帧找「回绕那一帧」（`data-arc` 骤降），记下这一帧的位移与落点偏差。
   *
   * 用途：证明**豁免掉的正是设计的瞬时落位**（落点在 guide 上），
   * 而不是「随便什么大跳都跳过」。
   */
  function collectSeamEvents(frames: number): Array<{
    key: string;
    arcDrop: number;
    step: number;
    gap: number;
  }> {
    const guides = guideByRouteKey();
    const events: Array<{ key: string; arcDrop: number; step: number; gap: number }> = [];
    let prev = truckSnap();
    for (let f = 0; f < frames; f++) {
      runFrame(1000 / 60);
      const cur = truckSnap();
      const gapped = new Map(gapsOfFrame(guides).map((r) => [r.key, r.gap]));
      for (const key of seamKeys(prev, cur)) {
        const a = prev.pos.get(key);
        const b = cur.pos.get(key);
        if (!a || !b) continue;
        events.push({
          key,
          arcDrop: (prev.arc.get(key) ?? 0) - (cur.arc.get(key) ?? 0),
          step: Math.hypot(a.x - b.x, a.y - b.y),
          gap: gapped.get(key) ?? Number.NaN,
        });
      }
      prev = cur;
    }
    return events;
  }

  it("回绕那一帧是「瞬时落位」：落点精确在 guide 上，且确实是单帧大位移", async () => {
    await mount(baseTopo());
    const events = collectSeamEvents(500);

    expect(events.length, "窗口里没有回绕，这条断言不成立").toBeGreaterThan(0);
    for (const e of events) {
      expect(e.arcDrop, `${e.key} 的判据本身`).toBeGreaterThan(SEAM_ARC_DROP);
      expect(
        e.step,
        `${e.key} 回绕帧的位移只有 ${e.step.toFixed(1)}px —— 说明它又被摊成滑行了`,
      ).toBeGreaterThan(SEAM_ARC_DROP);
      expect(
        e.gap,
        `${e.key} 回绕后落点偏离 guide ${e.gap.toFixed(3)}px（瞬时落位必须精确落在 P(arc) 上）`,
      ).toBeLessThan(0.2);
    }
  });

  it("豁免判据只认「回到入口」：几何重锚的弧长回退不算回绕（用实测数字）", () => {
    // 判据是纯函数，这里直接喂**实测到的真实数字**，不依赖「哪一帧恰好回绕」。
    // 数字来源：本几何下卡片宽 168→250 / 容器 +30% / 删出口 三种几何事件，
    // 以及一次真实回绕（`mixed#5`，与 resize 撞在同一帧）。
    const snap = (
      pos: Record<string, Pt>,
      arc: Record<string, number>,
    ): TruckSnap => ({ pos: new Map(Object.entries(pos)), arc: new Map(Object.entries(arc)) });

    // ① 真回绕：弧长 476.6 → 0.76（入口），屏幕位移 393.8px。
    expect(
      [...seamKeys(
        snap({ a: { x: 0, y: 0 } }, { a: 476.62 }),
        snap({ a: { x: 393.8, y: 0 } }, { a: 0.76 }),
      )],
      "真回绕必须被认出来",
    ).toEqual(["a"]);

    // ② 卡片宽度 168→250：实测各车弧长回退最大 82px —— 幅度就不够，不是回绕。
    expect(
      [...seamKeys(
        snap({ a: { x: 0, y: 0 } }, { a: 134.6 }),
        snap({ a: { x: 0.2, y: 0 } }, { a: 52.76 }),
      )],
      "几何重锚的回退幅度远小于一圈",
    ).toEqual([]);

    // ③ 假想的「大幅几何重锚」：弧长回退 350px 但落在**半路**（不在入口）——
    //    这正是加 `SEAM_MAX_ARC` 要挡住的情况：回绕一定把车放回一趟的入口。
    expect(
      [...seamKeys(
        snap({ a: { x: 0, y: 0 } }, { a: 600 }),
        snap({ a: { x: 1.5, y: 0 } }, { a: 250 }),
      )],
      "落在半路的大幅回退不得被当成回绕豁免",
    ).toEqual([]);
  });

  it("豁免只跳过真回绕那一辆车：几何重锚的滑行仍留在判据里（且被 cap 压住）", async () => {
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckSnap();

    layout.cardWidth = 250; // 与「几何变化」那一组同一个扰动
    fireResize();
    runFrame(1000 / 60);
    const after = truckSnap();

    // 这一帧**可能**恰好撞上一次真回绕（实测 `mixed#5` 就会）。允许 —— 但被跳过的
    // 那辆车必须确实是回绕：落回入口 + 单帧大位移。否则就是豁免被滥用了。
    const flagged = seamKeys(before, after);
    for (const k of flagged) {
      expect(after.arc.get(k) ?? -1, `${k} 被判成回绕却没落回入口`).toBeLessThan(SEAM_MAX_ARC);
      const p0 = before.pos.get(k)!;
      const p1 = after.pos.get(k)!;
      expect(
        Math.hypot(p0.x - p1.x, p0.y - p1.y),
        `${k} 被判成回绕却几乎没动`,
      ).toBeGreaterThan(SEAM_ARC_DROP);
    }

    // 豁免只跳过 `flagged` 这些车，其余一辆不少 —— 几何重锚的位移**仍在序列里**。
    const steps = stepsBetween(before, after);
    expect(steps.length, "豁免多吞了位移").toBe(before.pos.size - flagged.size);
    expect(steps.length, "没有可量的位移，判据不成立").toBeGreaterThan(0);
    // 几何变化造成的重锚位移确实还在（车真的动了），而且被 cap 压住（没被一起豁免）。
    expect(maxOf(steps), "几何变化这一帧车根本没动，判据不成立").toBeGreaterThan(0.05);
    expect(
      maxOf(steps),
      `几何变化的单帧位移 ${maxOf(steps).toFixed(2)}px 超过稳态 ${steady.toFixed(2)}px 的 5 倍`,
    ).toBeLessThan(5 * steady);
  });

  it("豁免是必要的、不是空壳：不豁免时 500 帧的尖峰比 > 2，豁免后 < 2", async () => {
    await mount(baseTopo());
    const exempt = collectSteps(500);
    const raw = collectSteps(500, 1000 / 60, { includeSeam: true });

    expect(median(raw), "车没在走，判据不成立").toBeGreaterThan(0.5);
    // 未豁免：回绕的单帧大位移就是尖峰 —— 这正是豁免存在的**理由**。
    expect(
      spikeRatio(raw),
      `未豁免的尖峰比 ${spikeRatio(raw).toFixed(1)}（回绕若不瞬时落位，这里不该是尖峰）`,
    ).toBeGreaterThan(2.0);
    // 豁免后：不算回绕那一帧，其余帧必须平滑。
    expect(
      spikeRatio(exempt),
      `豁免后的尖峰比 ${spikeRatio(exempt).toFixed(2)}（阈值 2.0）`,
    ).toBeLessThan(2.0);
  });

  it("瞬时落位后有 140ms 淡入，且会自己清掉（不是每帧都写 opacity）", async () => {
    await mount(baseTopo());
    // 推进到第一辆车回绕的那一帧。
    let prev = truckSnap();
    let wrapped: SVGGElement | null = null;
    for (let f = 0; f < 500 && !wrapped; f++) {
      runFrame(1000 / 60);
      const cur = truckSnap();
      const keys = [...seamKeys(prev, cur)];
      prev = cur;
      if (keys.length === 0) continue;
      wrapped = document.querySelector<SVGGElement>(
        `svg.flow g.flow__truck[data-truck-key="${keys[0]}"]`,
      );
    }
    expect(wrapped, "窗口里没有回绕，这条断言不成立").not.toBeNull();
    const g = wrapped!;
    const rect = g.querySelector("rect")!;

    // 回绕帧：淡入属性在，且不透明度 < 1（真的在淡入，不是空壳属性）。
    expect(g.getAttribute("data-wrap-fade")).not.toBeNull();
    expect(Number(rect.getAttribute("opacity"))).toBeLessThan(1);

    // ~140ms（9 帧）内跑完并**自己清掉**，不留每帧写属性的开销。
    for (let f = 0; f < 20; f++) runFrame(1000 / 60);
    expect(g.getAttribute("data-wrap-fade"), "淡入跑完没有清掉属性").toBeNull();
    expect(rect.getAttribute("opacity"), "淡入跑完没有恢复完全不透明").toBeNull();
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
    const before = truckSnap();

    await refresh(laneShapes(4.0 * GiB));
    runFrame(1000 / 60);
    const after = truckSnap();

    // 确认场景真的改变了车辆数量（否则这条测试什么也没考）。
    expect(after.pos.size).not.toBe(before.pos.size);
    expectNoTeleport(steady, stepsBetween(before, after));
  });

  it("新增出口（扇出变多）时，已在这条路线上的货车不得瞬移", async () => {
    // 防的故障：出口多一个 → 每条路线多两段分支（去 + 回）→ 路径长度与形状都变。
    // 进度仍是「占全程比例」，于是同一比例指向完全不同的位置（甚至换到别的分支）。
    // 布局高度变化会让真实 ResizeObserver 重测，这里显式补一次 fireResize()。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckSnap();
    const l0 = meanPathLength();

    await refresh(
      topoWith({ outbound: [...outlets(), outbound("node-b", "node", 0.3 * GiB, 1.0 * GiB)] }),
    );
    fireResize();
    runFrame(1000 / 60);

    const after = truckSnap();
    expect(after.pos.size).toBe(before.pos.size); // 车数不变，只有几何变了
    const l1 = meanPathLength();
    // 确认几何真的变了（否则这条测试无意义）。
    expect(Math.abs(l1 - l0) / l0).toBeGreaterThan(0.1);
    expectNoTeleport(steady, stepsBetween(before, after));
  });

  it("删除出口（旧 label 被卸载）时，货车不得瞬移", async () => {
    // 防的故障：出口被删 → 组件的测量闭包仍持有**已被卸载**的 label 元素引用，
    // 而浏览器对脱挂元素的 `getBoundingClientRect()` 返回全 0 → 那个 (0,0) 被当成
    // 真实锚点，整条扇出几何塌向原点 → 车跳。这正是「刷新重建 outlets 数组」
    // 那条怀疑的复现条件。用 fireResize() 模拟容器高度变化触发的重测。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckSnap();
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

    const after = truckSnap();
    expect(after.pos.size).toBe(before.pos.size);
    const l1 = meanPathLength();
    expect(Math.abs(l1 - l0) / l0).toBeGreaterThan(0.05);
    expectNoTeleport(steady, stepsBetween(before, after));
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
    const before = truckSnap();
    const l0 = meanPathLength();

    layout.cardWidth = 250;
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    expect(
      Math.abs(l1 - l0) / l0,
      `路径长度变化 ${(((l1 - l0) / l0) * 100).toFixed(1)}%`,
    ).toBeGreaterThan(0.2);
    expectNoTeleport(steady, stepsBetween(before, truckSnap()));
  });

  it("容器宽度 +30%（窗口缩放）时，货车不得瞬移", async () => {
    // 防的故障：窗口缩放 / 侧栏开合触发重新测量，路径被拉长。若进度只是比例，
    // 同一帧内所有车都会被整体挪动 —— 单帧位移达稳态的几十倍，看起来就是跳。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckSnap();
    const l0 = meanPathLength();

    layout.width = Math.round(820 * 1.3);
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    expect(Math.abs(l1 - l0) / l0, "任务书要求覆盖「路径长度变化 ±30%」").toBeGreaterThan(0.2);
    expectNoTeleport(steady, stepsBetween(before, truckSnap()));
  });

  it("几何只动一点点（≈3%）时，位移必须远小于稳态的 2 倍", async () => {
    // 防的故障：每 2 秒刷新带来的**小**几何抖动。大变化会让人以为是重排，
    // 小变化若也跳，就完全是「乱跳」的观感。这条是上界收紧版：
    // 小几何变化只允许产生小位移。
    await mount(baseTopo());
    const steady = steadyStep();
    const before = truckSnap();
    const l0 = meanPathLength();

    layout.cardWidth = 176;
    fireResize();
    runFrame(1000 / 60);

    const l1 = meanPathLength();
    const rel = Math.abs(l1 - l0) / l0;
    expect(rel).toBeGreaterThan(0.005);
    expect(rel).toBeLessThan(0.1);
    const steps = stepsBetween(before, truckSnap());
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
 * ⚠️ 这条**只是辅助**，主判据是 `measureArcInvariant()`（语义：车不得走进回程段）。
 *
 * 这些数字随修法变过：task-37 之后一圈 = 主干 + 一条分支 ≈ **0.21**；
 * 更早（`span` = 交错段序的前缀）≈ 0.6；`span = total`（走完整圈、含回程）≈ 1.0。
 * 所以阈值只够区分「远小于整环」与「接近整环」——**不要再拿它当主判据**：
 * 0.3–0.8 那条带子是按已经错掉的基线校准的。
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

/**
 * 语义不变量（task-37）：**车在任何一帧都不得落在回程段上**，且**每条分支都要有车经过**。
 *
 * 为什么不再用「一圈覆盖整环的百分比」当主判据：那个比例带（0.3–0.8）是**按有 bug 的
 * `span ≈ 60%` 校准**的 —— 拿按错误基线校准的代理指标当护栏，正是本项目反复栽的坑
 * （B1 那次也是：把修复回退掉，12/12 全绿、敏感度为 0）。
 *
 * 这里直接量**组件每帧写进 `data-arc` 的弧长位置**，再和「主干 + 各去程段」的区间比：
 * 回程段进一次就红。`data-arc` 也让「车在哪一段」不必靠屏幕位置反投影 ——
 * 回程段与去程段是**同一条曲线**，投影会「并列最近」。
 *
 * 顺带量「每条分支是否都被访问过」：老实现的 `span ≤ 1331.9` 永远到不了第三个出口，
 * 这条断言正是为它写的。
 */
function measureArcInvariant(): {
  frameCount: number;
  samples: number;
  offAllowed: number;
  branchCount: number[];
  visited: number[][];
} {
  const svg = document.querySelector("svg.flow");
  if (!svg) throw new Error("没有渲染 svg.flow");

  // 每条路线的段长（DOM 顺序）→ 主干 / 各去程段 / 各回程段 的弧长区间。
  const routes = [...svg.querySelectorAll("g[data-route-paths]")].map((g) => {
    const lens = [...g.querySelectorAll("path.flow__route")].map((p) => pathLengthOf(p));
    const trunk = lens[0] ?? 0;
    const allowed: [number, number][] = [[0, trunk]];
    const fwd: [number, number][] = [];
    const back: [number, number][] = [];
    let s = trunk;
    for (let k = 0; 2 * k + 1 < lens.length - 1; k++) {
      const f = lens[2 * k + 1] ?? 0;
      const b = lens[2 * k + 2] ?? 0;
      fwd.push([s, s + f]);
      allowed.push([s, s + f]);
      back.push([s + f, s + f + b]);
      s += f + b;
    }
    return { allowed, fwd, back };
  });

  const branches = Math.max(1, ...routes.map((r) => r.fwd.length));
  // 跑够「每条分支都轮一遍」：一圈 = TRAVEL_SECONDS，分支数 × 一圈 + 一点余量。
  const frameCount = Math.round(TRAVEL_SECONDS * 60 * (branches + 1));
  const visited: number[][] = routes.map((r) => r.fwd.map(() => 0));
  let samples = 0;
  let offAllowed = 0;
  const tol = 2; // px：边界取整误差

  for (let f = 0; f < frameCount; f++) {
    runFrame(1000 / 60);
    for (const g of svg.querySelectorAll("g.flow__truck") as NodeListOf<SVGGElement>) {
      const ri = Number(g.dataset.route ?? 0);
      const r = routes[ri];
      const arc = Number(g.dataset.arc);
      if (!r || !Number.isFinite(arc)) continue;
      samples++;
      const inAllowed = r.allowed.some(([a, b]) => arc >= a - tol && arc <= b + tol);
      const inBack = r.back.some(([a, b]) => arc > a + tol && arc < b - tol);
      if (!inAllowed || inBack) offAllowed++;
      r.fwd.forEach(([a, b], k) => {
        if (arc >= a - tol && arc <= b - tol) visited[ri]![k] = (visited[ri]![k] ?? 0) + 1;
      });
    }
  }
  return { frameCount, samples, offAllowed, branchCount: routes.map((r) => r.fwd.length), visited };
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
  it("车只在去程循环：任何一帧的弧长都不得落在回程段，且每条分支都要有车经过（task-37）", async () => {
    // 防两层故障：
    //  ①「车走回程」——老实现把「交错段序的前缀」当一圈，而前缀必然包含回程段
    //    （实测覆盖了整段 回₁ [484.0, 798.6]）；
    //  ②「车永远到不了后面的出口」——老实现 `span ≤ 1331.9 < 去₃ 起点 1448.8`，
    //    第三个出口那条线上永远没有车（算术可证，与弦长具体值无关）。
    //
    // 判据是**不变量本身**（车所在弧长必须落在「主干 + 某条去程段」里），不是
    // 「一圈覆盖整环的百分比」—— 那个比例带是按已经错掉的基线校准的。
    await mount(baseTopo());
    const m = measureArcInvariant();

    expect(m.samples, "没有任何带 data-arc 的车帧").toBeGreaterThan(100);
    expect(m.offAllowed, `有 ${m.offAllowed} 帧落在回程段（或允许区间之外）`).toBe(0);

    // 每个出口都必须有车经过（老实现第三个出口恒为 0）
    m.branchCount.forEach((n, ri) => {
      for (let k = 0; k < n; k++) {
        expect(m.visited[ri]![k] ?? 0, `第 ${ri} 条路线的第 ${k} 条分支没有任何一帧有车`).toBeGreaterThan(0);
      }
    });
  });

  it("辅助比例断言：一圈远小于整环（span 退回 total 会变红）", async () => {
    // **只是辅助**：它区分「≈整环」与「远小于整环」，但区分不了「主干 + 一条分支」
    // 与「主干 + 一条分支 + 半段回程」—— 后者由上面那条语义断言管。
    await mount(baseTopo());
    const ratios = measureLapTravel();

    expect(ratios.length).toBeGreaterThan(0);
    expect(Math.min(...ratios), "过低：车可能根本没在走").toBeGreaterThan(0.1);
    expect(median(ratios), `一圈走了 ${(median(ratios) * 100).toFixed(0)}%`).toBeLessThan(0.8);
    expect(maxOf(ratios), `最大 ${(maxOf(ratios) * 100).toFixed(0)}%`).toBeLessThan(0.85);
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


// ---------------------------------------------------------------------------
// 渲染后颜色（task-63）：车的 fill 必须与「它当前所在分支对应的出口类别」一致
//
// 为什么单列：现有 25 条拓扑测试里**没有任何一条读 `fill`**（grep 0 命中）。
// 也就是说「按状态取色」这套今天没有护栏 —— 改错取色/取错元素，测试会全绿。
//
// 为什么不 import `OUTBOUND_COLOR`：那样只是把「意图」抄第二遍，实现里的取色错了
// 测试会跟着一起错（本项目已发生过：B1 回退后全绿、`span=total` 回退后全绿）。
// 这里用**期望色字面量**（独立于实现），类别从 **DOM 的出口标签 class** 读、
// 分支区间从 **DOM 的可见线长度** 读、车的颜色从 **DOM 的 `fill`** 读。
// ---------------------------------------------------------------------------

/** 期望色（写死在测试里 = 独立于实现的金标）。 */
const GOLDEN_FILL: Record<string, string> = {
  node: "#4f8ef7",
  direct: "#34d399",
  block: "#f87171",
  dns: "#64748b",
  internal: "#64748b",
};

const KINDS = ["node", "direct", "block", "dns", "internal"] as const;

/** 出口标签（右列，非合计行）的类别，按 DOM 顺序 = 分支顺序。 */
function outletKindsFromDom(): string[] {
  return [...document.querySelectorAll(".highway__side--right .highway__lane-label")]
    .filter((el) => !el.classList.contains("highway__lane-label--total"))
    .map((el) => KINDS.find((k) => el.classList.contains(`highway__lane-label--${k}`)) ?? "");
}

interface BranchSpan {
  start: number;
  end: number;
  stroke: string;
}

/** 每条路线的「分支弧长区间」：从 DOM 的可见线（`path.flow__route`）长度累计，
 *  上色的那些就是去程分支（主干/回程是中性色）。顺序即出口顺序。 */
function routeBranchSpans(): { trunkEnd: number; branches: BranchSpan[] }[] {
  const out: { trunkEnd: number; branches: BranchSpan[] }[] = [];
  for (const grp of document.querySelectorAll("svg.flow g[data-route-paths]")) {
    const paths = [...grp.querySelectorAll("path.flow__route")] as SVGPathElement[];
    const trunkStroke = paths[0]?.getAttribute("stroke") ?? "";
    let acc = 0;
    let trunkEnd = 0;
    const branches: BranchSpan[] = [];
    paths.forEach((p, i) => {
      const len = pathLengthOf(p);
      const stroke = p.getAttribute("stroke") ?? "";
      if (i === 0) trunkEnd = acc + len;
      else if (stroke !== trunkStroke) branches.push({ start: acc, end: acc + len, stroke });
      acc += len;
    });
    out.push({ trunkEnd, branches });
  }
  return out;
}

interface TruckPaint {
  key: string;
  routeIdx: number;
  arc: number;
  fill: string;
}

/** 渲染后的颜色：读 `<rect>` 的真实 `fill` 属性（不是 `fill` 变量、不是常量）。 */
function truckPaints(): TruckPaint[] {
  const out: TruckPaint[] = [];
  for (const g of document.querySelectorAll("svg.flow g.flow__truck") as NodeListOf<SVGGElement>) {
    const key = truckKey(g);
    const arc = Number(g.dataset.arc);
    const rect = g.querySelector("rect");
    if (!Number.isFinite(arc) || !rect) continue;
    out.push({ key, routeIdx: Number(g.dataset.route ?? 0), arc, fill: rect.getAttribute("fill") ?? "" });
  }
  return out;
}

describe("渲染后颜色（读 DOM fill）", () => {
  it("车当前所在分支的 fill 必须等于该出口类别的既定色（覆盖 ≥2 种通道）", async () => {
    await mount(baseTopo());
    const allKinds = outletKindsFromDom();
    // 流程图只画「流向用户」的出口；dns/api 是内部通道，不在流程图上（另一条测试钉着这点）。
    const kinds = allKinds.filter((k) => k !== "dns" && k !== "internal");
    expect(kinds.length, "没有读到出口标签类别").toBeGreaterThanOrEqual(3);

    const spans = routeBranchSpans();
    expect(kinds.length, "流程图的分支数与出口类别数不一致").toBe(spans[0]!.branches.length);
    const bad: string[] = [];
    const byKind = new Map<string, number>();
    let sampled = 0;

    for (let f = 0; f < 900; f++) {
      runFrame(1000 / 60);
      for (const t of truckPaints()) {
        const branches = spans[t.routeIdx]?.branches ?? [];
        // 车在主干上时，fill 指向「这一趟要送的分支」——仅凭 arc 无法确定是哪条 → 跳过
        // 只采「分支内部」：两端各留 0.5px，避开主干↔分支边界（在边界上 arc 属于主干，
        // 而 fill 已经切到「这一趟要送的分支」，会误报）
        const bi = branches.findIndex((b) => t.arc >= b.start + 0.5 && t.arc < b.end - 0.5);
        if (bi < 0) continue;
        const kind = kinds[bi];
        const want = kind ? GOLDEN_FILL[kind] : undefined;
        if (!kind || !want) {
          bad.push(`分支 ${bi} 没有可判定的出口类别（kinds=${JSON.stringify(kinds)}）`);
          continue;
        }
        sampled += 1;
        byKind.set(kind, (byKind.get(kind) ?? 0) + 1);
        // 同时校验「可见线本身的颜色」与金标一致：否则取色写错时两边一起错、仍然自洽
        if (branches[bi]!.stroke !== want) {
          bad.push(`第 ${bi} 条分支线 stroke=${branches[bi]!.stroke} 应为 ${want}（kind=${kind}）`);
        }
        if (t.fill !== want) {
          bad.push(`key=${t.key} arc=${t.arc.toFixed(1)} 在分支 ${bi}(${kind}) 上 fill=${t.fill} 应为 ${want}`);
        }
      }
    }

    expect(sampled, "没有采样到「车在分支上」的帧，断言是空壳").toBeGreaterThan(200);
    expect(byKind.size, `只观察到 ${byKind.size} 种通道，颜色切换没被覆盖`).toBeGreaterThanOrEqual(2);
    // 先报前几条，避免一个断言刷屏
    expect(bad.slice(0, 6), `颜色不符 ${bad.length} 处`).toEqual([]);
  });
});
