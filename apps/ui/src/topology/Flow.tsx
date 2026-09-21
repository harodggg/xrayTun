/**
 * 拓扑流向图的**动画核心**：连线 + 沿连线行走的货车。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14 步骤 D，纯搬迁、行为不变）。
 *
 * ⚠️ 这里是前几轮反复修出来的三条不变量（车只在去程循环、`data-truck-key`
 * 稳定身份、按上一帧屏幕点重锚 + `walkRef` 限速）所在地。**逐字复制，
 * 不要"顺手简化"** —— 尤其 `routeToD` 里「每条支线后补一段回分叉点」那种
 * 看着冗余的结构（那是在修「车有 1/4 时间飞在空白处」）。
 *
 * 已知边界：`topologyAnimation.test.ts` 对「只在去程循环」与「稳定身份」
 * **没有断言**（task-36 补）。所以本文件的忠实性靠逐行 diff 证明，不靠测试。
 */

import { useEffect, useRef, useState } from "react";
import type { MutableRefObject } from "react";

import type { TopoInbound, TopoOutbound } from "../types";
import {
  NEUTRAL,
  OUTBOUND_COLOR,
  TRAVEL_SECONDS,
  clampMid,
  routeToD,
  trucksOnLane,
} from "./flowGeometry";
import type { Rel, Route, Seg, TruckState } from "./flowGeometry";

/**
 * 回绕（「这趟送到了」）之后货车淡入的时长。
 *
 * 回绕那一帧是**瞬时落位**（见下面 `wrapped` 处）：车从分支末端直接出现在主干起点，
 * 跨度 ~486px。纯突变看起来像掉帧/闪一下；140ms 的不透明度斜坡让这一下读得出
 * 「上一趟送达、下一趟开始」。刻意**不用 CSS 动画/定时器**：步进由动画循环自己的
 * `dt` 驱动，所以手动时钟的测试里也是确定的。
 */
const WRAP_FADE_SECONDS = 0.14;

/**
 * 在「主干 + 当前分支」这两段弧长区间里找离 `(px, py)` 最近的点（几何变化后重锚用）。
 *
 * 为什么要限定区间：guide 的段序里夹着回程段，而回程段与去程段**几何重合**；
 * 全局最近点搜索可能把车重锚到回程段上（那样它下一帧就走回程了）。
 * 只扫允许的两段，落点一定在「车本来就该走的路」上。
 */
function nearestOnOutbound(
  path: SVGPathElement,
  trunk: number,
  branch: { start: number; len: number },
  px: number,
  py: number,
): number {
  let best = 0;
  let bestD = Infinity;
  const scan = (from: number, to: number) => {
    if (!(to > from)) return;
    const step = Math.max(2, (to - from) / 64);
    for (let s = from; s <= to; s += step) {
      const p = path.getPointAtLength(s);
      const dx = p.x - px;
      const dy = p.y - py;
      const d = dx * dx + dy * dy;
      if (d < bestD) {
        bestD = d;
        best = s;
      }
    }
  };
  scan(0, trunk);
  scan(branch.start, branch.start + branch.len);
  return best;
}

/**
 * 网络流动：连线 + 沿连线行走的货车。
 *
 * # 为什么连线和货车画在同一层
 *
 * 早期把「车道」和「连线」分成两层：车道是带货车的容器、连线是覆盖层。
 * 结果是**货车只在自己那条车道里左右漂**，和连线没有关系 ——
 * 看着像两套不相干的东西。
 *
 * 现在一条路线就是**一条合并路径**（入口 → 车道 → 扇出 → 出口），
 * 货车沿它从头走到尾；车道那一列只作为「道路」的视觉底衬。
 *
 * # 「不跳」在这里的准确含义（三条不变量）
 *
 * 1. **身份稳定**：车的身份是 `路线key#序号`（挂 `data-truck-key`），
 *    不是数组下标。车辆数量一变，按下标存的进度会让后面的车整体错位、
 *    带着旧进度落到**别人的路线**上（实测中位 44.8px vs 稳态 4.4px）。
 * 2. **进度按绝对路程（px）存**，不按「占全程的比例」。比例会随路径长度变化
 *    而指向别处：出口增减 / 容器缩放都会让同一比例落到完全不同的屏幕点。
 * 3. **几何变化时重锚 + 限速**：路径重算后用「上一帧的屏幕点」在新路径上
 *    取最近点作为新路程（位移最小），且单帧位置修正不超过正常车速的 1.5 倍 ——
 *    几何突变时让车**走**过去，而不是一帧瞬移过去。
 *
 * # 一处边界
 *
 * 「哪条车道通向哪个出口」核心**没有这个计数器**（只有按入口、按出口两类
 * 统计）。所以出口按相邻切段分配到车道，只为了**画得清楚**；
 * 它表达「车道与出口相连」这个结构关系（来自配置），不声称逐条归属。
 */
export function Flow({
  container,
  inbound,
  outbound,
  inletRefs,
  outletRefs,
  trafficOk,
  highlight,
}: {
  container: HTMLElement | null;
  inbound: TopoInbound[];
  outbound: TopoOutbound[];
  /**
   * 直接拿 ref（而不是 `ref.current` 的**快照数组**）：快照是**渲染时**取的，
   * 而 ref 在 commit 时才更新。入口/出口集合变化的那个渲染里，快照仍指向旧元素，
   * 测量就会拿到已卸载的卡片（rect 全 0）。读 `.current` 永远是最新的。
   */
  inletRefs: MutableRefObject<(HTMLElement | null)[]>;
  outletRefs: MutableRefObject<(HTMLElement | null)[]>;
  /** 流量是否可信；`false` 时字节字段是占位 0，不能当读数用。 */
  trafficOk: boolean;
  /**
   * 单连接高亮：`{ inlet, outlet }` 是入口/出口 tag。两者都命中时画一条
   * 加亮弧线（主干 + 那一段分支）。**不参与动画**，纯粹是叠加层。
   */
  highlight: { inlet: string; outlet: string } | null;
}) {
  const [geo, setGeo] = useState<{
    w: number;
    h: number;
    routes: Route[];
  } | null>(null);
  /**
   * 上一帧的时间戳。用**时间增量累积**推进度，而不是「绝对时间 × 速度」。
   *
   * 为什么：拓扑每 2 秒刷新一次，入口卡片的宽度会变 → 路径长度变。
   * 若按绝对时间算比例 `u = t·v/total`，长度一变，同一时刻的 `u` 就跳 ——
   * 表现就是车在路上跳。改成累积路程后，长度变化只影响「占全程的比例」，
   * 走过的**绝对距离**是连续的。
   */
  const prevRef = useRef<number | null>(null);
  /**
   * 每辆车的状态，按**稳定身份**（`data-truck-key`）保存 —— 不按数组下标。
   * 跨 effect 重建连续。
   */
  const progressRef = useRef<Map<string, TruckState>>(new Map());
  /**
   * 最后一次**可信**的入口字节数（按入口 tag）。
   * `traffic_ok === false` 时拿它继续算车数：否则每次查询失败，车数都会
   * 从 13 掉到 9 再弹回来，那是另一种「乱跳」。
   */
  const lastBytesRef = useRef<Map<string, number>>(new Map());

  /**
   * 入口/出口的**身份集合**。测量 effect 必须在它变化时重跑：
   * 早先 effect 只依赖 `[container]`，闭包长期持有旧元素数组 ——
   * 删掉一个入口后重测，还会拿**已卸载**的卡片（rect 全 0）当锚点，
   * 整条扇出塌向原点（实测 guide 路径 2581px → 5585px，单帧位移 353.8px）。
   */
  const topoSig = `${inbound.map((i) => i.tag).join("|")}=>${outbound.map((o) => o.tag).join("|")}`;

  // 测量：把 DOM 位置换算成「相对容器的坐标」，再拼出每条路线的路径
  useEffect(() => {
    if (!container) return;
    const measure = () => {
      const base = container.getBoundingClientRect();
      const rel = (el: HTMLElement | null | undefined): Rel | null => {
        // 已从文档移除、或还没布局的元素会给出**全 0** 的 rect。
        // 把它当锚点会让整条扇出塌向 (0,0) —— 实测 guide 路径 2581px → 5585px。
        if (!el || !el.isConnected) return null;
        const r = el.getBoundingClientRect();
        if (r.width === 0 && r.height === 0) return null;
        return {
          l: r.left - base.left,
          r: r.right - base.left,
          y: r.top - base.top + r.height / 2,
        };
      };
      // **按下标对齐**：inlets[i] ↔ inbound[i]、outlets[j] ↔ outbound[j]，空位保留为 null。
      // 早先用 `compact` 把空位挤掉，于是「中间少一个」（比如 DNS 出口被删掉）时，
      // 后面的出口会串到别人的 kind 上，颜色与字节也跟着错位。
      const inlets = inbound.map((_, i) => rel(inletRefs.current[i]));
      const outlets = outbound.map((_, j) => rel(outletRefs.current[j]));
      const firstInlet = inlets.find((x): x is Rel => x !== null);
      const firstOutlet = outlets.find((x): x is Rel => x !== null);
      if (!firstInlet || !firstOutlet) {
        setGeo({ w: base.width, h: base.height, routes: [] });
        return;
      }

      // 分叉点：放在入口与出口之间的**流动区**里。
      //
      // 系数 0.35 = 汇入占 35%、扇出占 65%：扇出有 3×5 = 15 条曲线，
      // 汇入只有 3 条。早先取 0.7 时实测分支区只有 145×277px（宽高比 0.52，
      // 外侧支线几乎是竖线），而左侧 70% 的空隙只跑 3 条水平主干 —— 空间分配正好反了。
      // 改成 0.35 后分支区约 315×277（宽高比 1.14），扇面横向摊开一倍。
      const gapStart = firstInlet.r;
      const gapEnd = firstOutlet.l;
      const forkX = gapStart + (gapEnd - gapStart) * 0.35;

      const routes: Route[] = [];
      inlets.forEach((inlet, i) => {
        // 这张卡片这一轮量不到（还没布局）：先不画它。
        // 注意不能因此挤掉下标 —— `key` 用入口 tag，下一次测量它会自己回来。
        if (!inlet) return;
        // **一对多**：每个入口都扇出到**全部**出口。
        //
        // 物理上这也更贴近事实：所有入口的流量都经过同一条规则链，
        // 由规则链决定去向，所以任何入口都可能去任何出口。
        // 早先按相邻切段分（一条入口只连自己那几个），看起来像「每个入口
        // 有自己独立的一组出口」，那是不对的。
        //
        // 代价是线条数 = 入口数 × 出口数（当前 3×6 = 18 条），所以线的透明度
        // 压低、主视觉留给货车。
        // **每条分支「去 + 回」成对插入**，而不是「先去完所有出口再一起回来」。
        //
        // 这是「车走在可见线上」的前提：SVG 的 `C`（三次贝塞尔）从**上一段的终点**
        // 继续，只有相邻两段满足 `seg[i].x1 === seg[i-1].x2`（且 y 相同）时，
        // 合并路径的几何才等于那些可见线的几何。
        // 早先把所有回程堆在最后，于是「去第 2 个出口」的曲线从**第 1 个出口**
        // 出发 —— 实测量到 46.7% 的行程离任何可见线 >1.5px、最大偏离 23.8px，
        // 也就是用户最早说的「车的路径不对」。
        const trunk: Seg = {
          kind: "curve",
          x1: inlet.r,
          y1: inlet.y,
          x2: forkX,
          y2: inlet.y,
          cx: (inlet.r + forkX) / 2,
        };
        const trunkBack: Seg = {
          kind: "curve",
          x1: forkX,
          y1: inlet.y,
          x2: inlet.r,
          y2: inlet.y,
          cx: (inlet.r + forkX) / 2,
        };
        const pairs: { fwd: Seg; back: Seg; color: string; tag: string }[] = [];
        outlets.forEach((b, k) => {
          if (!b) return;
          // 类别要取**出口对象**上的 kind（位置矩形里没有这个信息）
          const color = OUTBOUND_COLOR[outbound[k]?.kind ?? ""] ?? NEUTRAL;
          pairs.push({
            color,
            // 单连接高亮按这个 tag 找分支（与日志 `[入站 -> 出站]` 右侧同一个值）
            tag: outbound[k]?.tag ?? "",
            fwd: {
              kind: "curve",
              x1: forkX,
              y1: inlet.y,
              x2: b.l,
              y2: b.y,
              cx: clampMid(forkX, b.l),
              color,
            },
            back: {
              kind: "curve",
              x1: b.l,
              y1: b.y,
              x2: forkX,
              y2: inlet.y,
              cx: clampMid(forkX, b.l),
            },
          });
        });
        // 依次拼接：主干 → (去出口1 → 回分叉) → (去出口2 → 回分叉) → … → 回入口。
        // 每一段的终点就是下一段的起点，所以合并路径等于**可见线的并集**，
        // 且首尾重合（闭环）。
        //
        // ⚠️ 段序里夹着回程段，它只有一个用途：让「导引路径」与可见线完全重合
        // （B1 修复：车曾有约 1/4 行程飞在空白处）。**车的行程不再从这条弧序里
        // 取前缀** —— 交错段序的前缀必然包含回程段，于是车会走整段回程，而且
        // 永远到不了后面的出口（task-37 实测：`span ≤ 1331.9 < 去₃ 起点 1448.8`）。
        // 车走哪些段见动画里的「一圈 = 主干 + 一条分支」。
        const segs: Seg[] = [trunk];
        const branches: Route["branches"] = [];
        for (const p of pairs) {
          branches.push({ color: p.color, tag: p.tag, fwd: p.fwd });
          segs.push(p.fwd, p.back);
        }
        segs.push(trunkBack);

        routes.push({
          // 稳定身份 = 入口 tag：入口顺序/数量变化时，车的身份不跟着漂。
          key: inbound[i]!.tag,
          d: routeToD(segs),
          segs,
          /** 这条路线属于哪个入口（货车数量按入口字节数决定）。 */
          inlet: i,
          trunk,
          branches,
        });
      });

      setGeo({ w: base.width, h: base.height, routes });
    };

    measure();
    // 流量每 2 秒刷新一次，卡片宽度/行数会变 —— 持续跟随，而不是量一次就完
    const ro = new ResizeObserver(measure);
    ro.observe(container);
    for (const el of [...inletRefs.current, ...outletRefs.current]) {
      if (el) ro.observe(el);
    }
    return () => ro.disconnect();
    // 依赖里**不放位置数组**：它们每次渲染都会重建，放了会让动画每 2 秒重启一次
    // （表现是车跳回起点）。几何更新由 ResizeObserver 触发；
    // 入口/出口的**身份集合**变化时（topoSig 变）必须重跑，否则闭包会一直
    // 拿着旧元素数组（含已卸载的卡片）去测量。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [container, topoSig]);

  // 沿路径行走：用 `getPointAtLength` 把货车摆到路上。
  // 不用 state 驱动（每帧 setState 会把整棵树重渲染），直接改 transform。
  useEffect(() => {
    if (!container || !geo || geo.routes.length === 0) return;
    const routeIdxByKey = new Map(geo.routes.map((r, i) => [r.key, i]));
    // 用容器里那条隐藏的**完整合并路径**算位置（可见的线是分段画的，长度不等于整条）。
    // **直接按下标取 DOM**：早先用 `guideRefs.current.filter(Boolean)` 把空位挤掉了，
    // 删掉一条路线后下标被压缩，剩下的货车会读到**别的路线**的路径。
    const guides = [...container.querySelectorAll<SVGPathElement>("path.flow__guide")];
    const totals = guides.map((p) => {
      try {
        return p.getTotalLength();
      } catch {
        return 0;
      }
    });
    const sigs = geo.routes.map((r) => r.d);

    // 每辆车 `<g>` 里那个 `<rect>` 的**按 key 缓存**（与上面的 `spansByKey` 同一手法）。
    //
    // 为什么：动画每帧对每辆车做一次 `g.querySelector("rect")` —— 实测 11.03 次/帧
    // （11 辆车时 ≈662/s，task-56/63）。它每帧的结果**永远不变**，纯属热路径上的重复查询。
    //
    // 为什么还要 `isConnected` 兜底：车辆数量变化时，同名 key 可能对应**新**元素
    // （旧 `<g>` 已随 React 卸载）。往缓存里那个脱挂的元素写属性等于没写 ——
    // 表现就是那辆车的颜色/淡入停在初始值。所以缓存命中也要确认它还挂在文档上。
    const rectByKey = new Map<string, SVGRectElement>();
    for (const g of container.querySelectorAll<SVGGElement>("g.flow__truck")) {
      const key = g.dataset.truckKey;
      const r = g.querySelector("rect");
      if (key && r) rectByKey.set(key, r);
    }

    // 每条路线「主干 + 各分支去程段」在 guide 上的**真实弧长区间**。
    //
    // 必须用引擎量的弧长（`getTotalLength`），不能用弦长：车的落点由
    // `getPointAtLength(dist)` 给出，单位是弧长；用弦长会在段边界错位。
    // 段序是 `主干, 去₁, 回₁, 去₂, 回₂, …, 回主干`，所以第 k 条分支的去程段
    // 在弧长上从 `主干 + Σ(去ⱼ + 回ⱼ)` 开始（要跳过它前面的回程段）。
    const spansByKey = new Map<string, { trunk: number; branches: { start: number; len: number }[] }>();
    for (const group of container.querySelectorAll<SVGGElement>("g[data-route-paths]")) {
      const key = group.dataset.routePaths;
      if (!key || spansByKey.has(key)) continue;
      const lens = [...group.querySelectorAll<SVGPathElement>("path.flow__route")].map((p) => {
        try {
          return p.getTotalLength();
        } catch {
          return 0;
        }
      });
      const trunk = lens[0] ?? 0;
      const branches: { start: number; len: number }[] = [];
      let start = trunk;
      // 去程段落在奇数下标；最后一段是「回主干」，不算分支。
      for (let k = 0; 2 * k + 1 < lens.length - 1; k++) {
        const fwd = lens[2 * k + 1] ?? 0;
        branches.push({ start, len: fwd });
        start += fwd + (lens[2 * k + 2] ?? 0);
      }
      if (trunk > 0 && branches.length > 0) spansByKey.set(key, { trunk, branches });
    }

    let raf = 0;
    const step = (now: number) => {
      const prev = prevRef.current;
      prevRef.current = now;
      let dt: number;
      if (prev === null) {
        dt = 1 / 60; // 这一轮动画的第一帧：按标称帧长走一步
      } else {
        dt = (now - prev) / 1000;
        if (!Number.isFinite(dt) || dt < 0 || dt > 0.1) {
          // 掉帧 / 切标签回来：**不补算**中间的停顿，否则会在那一帧跳很远
          dt = 0;
        } else if (dt > 1 / 30) {
          // 一帧里最多按 2 帧补：主线程卡 100ms 时补满会一步走 37px，
          // 观感就是「跳一下」；宁可让动画在那几帧里走慢一点。
          dt = 1 / 30;
        }
      }

      const state = progressRef.current;
      // 每帧按 DOM 取车：车辆数量变化时 React 会增删节点，这里自然跟上，
      // 不需要任何按下标的数组（那正是错位的来源）。
      for (const g of container.querySelectorAll<SVGGElement>("g.flow__truck")) {
        const key = g.dataset.truckKey;
        if (!key) continue;
        const idx = routeIdxByKey.get(g.dataset.routeKey ?? "");
        if (idx === undefined) continue; // 这条路线这轮没画出来：等下一轮测量
        const path = guides[idx];
        const total = totals[idx] ?? 0;
        if (!path || !(total > 0)) continue;
        const route = geo.routes[idx]!;

        const spans = spansByKey.get(route.key);
        if (!spans) continue; // 还没量到分段弧长：这一轮先不动这辆车
        const nB = spans.branches.length;

        let st = state.get(key);
        if (!st) {
          const phase = Number(g.dataset.phase ?? 0);
          st = {
            dist: (Number.isFinite(phase) ? phase : 0) * (spans.trunk + spans.branches[0]!.len),
            branch: 0,
            x: NaN,
            y: NaN,
            sig: sigs[idx]!,
            walkRef: 0,
          };
          state.set(key, st);
        }
        // 出口被删掉时分支数会变少：把下标收敛回合法范围。
        if (st.branch >= nB) st.branch = 0;

        // 一圈 = **主干 + 某一条分支**：送到一个出口就算一趟，下一趟换下一条分支。
        //
        // 这是 task-37 的修法（方案 C）。老实现把「交错段序的前缀」当一圈，而前缀
        // 必然包含回程段 → 车走整段回程；而且 `span ≤ 1331.9` 永远小于第三个出口的
        // 起点 1448.8 → **第三个出口永远没有车经过**（真实 DOM 量出来的）。
        // 现在车在任何一帧都只落在「主干」或「某一条分支的去程段」上；回程段只用来
        // 让导引路径与可见线重合（B1 修复），车一步也不走它。
        let br = spans.branches[st.branch]!;
        let lap = spans.trunk + br.len;
        if (!(lap > 0)) continue;
        const walk = (lap / TRAVEL_SECONDS) * dt;

        // 几何变了（这条路的 `d` 变了）→ 用**上一帧的屏幕点**在新路径上取最近点重锚，
        // 而且只在「主干 + 当前分支」这两段里找 —— 免得重锚把车丢到回程段上。
        if (st.sig !== sigs[idx]) {
          if (Number.isFinite(st.x) && Number.isFinite(st.y)) {
            st.dist = nearestOnOutbound(path, spans.trunk, br, st.x, st.y);
          }
          st.sig = sigs[idx]!;
        }

        st.dist += walk;
        // 送完这一个出口 → 换下一条分支、从主干起点重新开始。
        // 「从出口跳回入口」那一帧语义就是「这趟货送到了」。
        //
        // task-51：这一帧**瞬时落位**（跳过下面的屏幕直线限速）。
        // 限速器做的是屏幕空间线性插值，而回绕的跨度天生就是「分支末端 → 主干起点」
        // ≈486px（真机实测 lap 弦 484–505px）——把它摊平等于每圈花 ~2.7s 沿一条直线
        // 慢慢飞回入口，滑行期间车离开可见线最远 **33.85px**、滑行帧 48.14% 离线
        // （正常行走帧只有 3.11%），一圈里约 40% 的时间都在滑。用户看到的
        // 「（蓝车）漂移」就是它：node 分支最长 → 回绕跨度最大 → 幅度最大。
        // 送到了就该在入口出现，而不是慢慢挪回去。
        //
        // 几何变化（resize / 出口增减）**仍然走限速滑行** —— 那才是 cap 当初的目的。
        let wrapped = false;
        if (st.dist >= lap) {
          st.dist -= lap * Math.floor(st.dist / lap);
          st.branch = (st.branch + 1) % nB;
          br = spans.branches[st.branch]!;
          lap = spans.trunk + br.len;
          wrapped = true;
          g.setAttribute("data-wrap-fade", "0"); // 下一帧起 140ms 淡入
        }

        // 把「这一圈的路程」映射到 guide 的**弧长**：
        //   dist < 主干长 → 就在主干上；
        //   否则 → 第 branch 条分支的去程段（跳过它前面的所有回程段）。
        // 用引擎量的真实弧长，所以落点与 `getPointAtLength` 一致。
        const arc = st.dist < spans.trunk ? st.dist : br.start + (st.dist - spans.trunk);
        const target = path.getPointAtLength(arc);
        let nx = target.x;
        let ny = target.y;
        let capped = false;
        // 回绕那一帧不做屏幕插值：直接落到 `target`（= P(arc)）。
        if (!wrapped && Number.isFinite(st.x) && Number.isFinite(st.y)) {
          const gap = Math.hypot(target.x - st.x, target.y - st.y);
          // 位置修正速度上限 = **正常行走速度**的 1.5 倍（且不低于这一帧的正常步长）。
          // 几何突变（窗口缩放 / 出口增减）时车**匀速**走过去，而不是一帧跳过去。
          //
          // 参考量必须取「正常行走时的步长」并且**在被限速的帧上不更新**：
          // 早先拿「上一帧实际走了多远」当参考，限速帧本身会成为下一帧的参考，
          // 于是上限每帧 ×1.5 指数增长 —— 看起来是车先慢慢挪、再越挪越快，几帧内冲过去。
          const cap = Math.max(1.5 * st.walkRef, walk);
          if (gap > cap) {
            const k = cap / gap;
            nx = st.x + (target.x - st.x) * k;
            ny = st.y + (target.y - st.y) * k;
            capped = true;
          }
        }
        const stepLen = Number.isFinite(st.x) ? Math.hypot(nx - st.x, ny - st.y) : walk;
        // 只有正常行走才更新参考步长。
        // 回绕帧的 `stepLen` 是 ~486px 的瞬时位移，**绝不能**当参考 —— 否则下一帧的
        // cap 变成 1.5×486，限速等于失效（这正是「参考量必须在被限速的帧上不更新」
        // 那条注释防的同一个坑，回绕帧是它的第二个入口）。
        if (!capped && !wrapped) st.walkRef = stepLen;
        st.x = nx;
        st.y = ny;
        g.setAttribute("transform", `translate(${nx.toFixed(1)} ${ny.toFixed(1)})`);
        // 车在 guide 上的**弧长位置**：既是给护栏的准确落点，也让探针不必再从屏幕
        // 位置反投影 —— 回程段与去程段是**同一条曲线**，投影会「并列最近」。
        g.dataset.arc = arc.toFixed(2);

        // 车**不走回程**：一圈 = 主干 + 一条分支（见上面的 `lap` / `arc`），
        // 所以不存在「倒着开」。这里只是兜底清掉可能残留的隐藏状态。
        if (g.style.visibility === "hidden") g.style.visibility = "";

        // 颜色跟着**这一圈要送的分支**走（一趟一个颜色），不再按「整圈比例」猜。
        // `<rect>` 从表里取（见上面的 `rectByKey`）：miss 或已脱挂时才查一次 DOM，
        // 所以稳态下这条路径**没有** `g.querySelector("rect")`。
        let rect = rectByKey.get(key);
        if (!rect || !rect.isConnected) {
          rect = g.querySelector("rect") ?? undefined;
          if (rect) rectByKey.set(key, rect);
        }
        if (rect) {
          const fill = route.branches[st.branch]?.color || NEUTRAL;
          if (fill !== st.color) {
            rect.setAttribute("fill", fill);
            st.color = fill;
          }
          // 回绕后的淡入（见 WRAP_FADE_SECONDS）。进度放在 `<g>` 自己的属性上：
          // 它纯粹是渲染细节，没必要进 `TruckState`（那是轨迹状态，属于
          // flowGeometry.ts 的契约）；而且节点重建时它自然跟着重置。
          const rawFade = g.getAttribute("data-wrap-fade");
          if (rawFade !== null) {
            const v = Math.min(1, Number(rawFade) + dt / WRAP_FADE_SECONDS);
            if (v >= 1) {
              g.removeAttribute("data-wrap-fade");
              rect.removeAttribute("opacity");
            } else {
              g.setAttribute("data-wrap-fade", v.toFixed(3));
              rect.setAttribute("opacity", v.toFixed(3));
            }
          }
        }
      }
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => {
      cancelAnimationFrame(raf);
      prevRef.current = null; // 下一轮从「现在」接着走，不补算中间的停顿
    };
    // 依赖只放「测量结果」与容器：位置数组每次渲染都会重建，放进来会让动画
    // 每 2 秒重启一次（表现是车跳回起点）。
  }, [geo, container]);

  if (!geo) return null;

  /**
   * 一个入口用于**决定车数**的字节数。
   *
   * 流量不可信时（`traffic_ok === false`）继续用最后一次可信读数，
   * 而不是 0：否则每次查询失败车数都会 13 → 9 → 13 地弹，那也是一种乱跳。
   */
  const laneBytes = (lane: TopoInbound | undefined): number => {
    if (!lane) return 0;
    const total = lane.uplink_bytes + lane.downlink_bytes;
    if (trafficOk) {
      lastBytesRef.current.set(lane.tag, total);
      return total;
    }
    return lastBytesRef.current.get(lane.tag) ?? 0;
  };

  // 每辆货车：数量按**入口**的字节数决定，沿属于该入口的那条路线来回走。
  // 身份 = `路线key#序号`：车辆数量变化时，已有车的身份、路线、进度都不变。
  const trucks: { key: string; routeKey: string; routeIdx: number; phase: number }[] = [];
  geo.routes.forEach((r, idx) => {
    const count = trucksOnLane(laneBytes(inbound[r.inlet]));
    for (let k = 0; k < count; k++) {
      trucks.push({
        key: `${r.key}#${k}`,
        routeKey: r.key,
        routeIdx: idx,
        phase: (k / count + (k % 3) * 0.04) % 1,
      });
    }
  });

  // 单连接高亮路径：主干 + 该出口那一段。**在 render 里按 tag 现算**，
  // 不进 geo（几何测量与动画核心完全不碰它）。
  const highlightD = (() => {
    if (!highlight) return null;
    const route = geo.routes.find((r) => r.key === highlight.inlet);
    const branch = route?.branches.find((b) => b.tag === highlight.outlet);
    if (!route || !branch) return null;
    return routeToD([route.trunk, branch.fwd]);
  })();

  return (
    <svg className="flow" width={geo.w} height={geo.h} aria-hidden>
      {/* 连线本体：主干中性，各扇出分支按**目的地的出口类别**着色。
          着色依据是出口的 kind（实测数据），所以图例对得上。 */}
      {geo.routes.map((r) => (
        <g key={`p-${r.key}`} data-route-paths={r.key}>
          {r.segs.map((sg, k) => (
            <path
              key={`p-${r.key}-${k}`}
              className={sg.color ? "flow__route" : "flow__route flow__route--trunk"}
              d={routeToD([sg])}
              stroke={sg.color ?? "rgba(120,160,210,0.45)"}
            />
          ))}
        </g>
      ))}
      {/* 单连接高亮：画在连线之上、货车之下。虚线动画给出「往哪边走」的方向感。 */}
      {highlightD && <path className="flow__highlight" d={highlightD} data-highlight="1" />}
      {/* 唯一一条**完整**的合并路径：只用于给货车算位置（隐藏不显示） */}
      {geo.routes.map((r) => (
        <path key={`guide-${r.key}`} className="flow__guide" d={r.d} />
      ))}
      {/* 货车：沿整条路线（入口 → 分叉 → 各出口）来回走，颜色跟着当前去向。
          `data-truck-key` 是**稳定身份**，跨刷新用它认「同一辆车」。 */}
      {trucks.map((t, i) => (
        <g
          key={t.key}
          data-truck-key={t.key}
          data-route-key={t.routeKey}
          data-route={t.routeIdx}
          data-phase={t.phase}
          data-slot={i}
          className="flow__truck"
        >
          <rect x={-4.5} y={-2.5} width={9} height={5} rx={1} fill={NEUTRAL} />
        </g>
      ))}
    </svg>
  );
}
