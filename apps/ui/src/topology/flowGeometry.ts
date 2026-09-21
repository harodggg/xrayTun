/**
 * 拓扑流向图的**几何与常量**：出口着色、车数映射、锚点/线段/路线类型，
 * 以及「沿合并路径行走」用到的路径几何辅助（取最近点、闭环拼接、分段长度）。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14 步骤 B，纯搬迁、行为不变）。
 *
 * ⚠️ 这里的结构与数值是前几轮反复修出来的（车只在去程循环、几何按
 * `data-truck-key` 重锚、`routeToD` 每条支线后补一段回分叉点）。
 * 搬运时**逐字复制**，不要"顺手简化"。
 */

/**
 * 哪些出口属于**内部通道**（不是流向用户的去向）。
 *
 * * `dns` —— 核心的 `dns-out`，处理被劫持的 DNS 查询（UDP）；
 * * `internal` —— `api`，本机回环，用于读统计与自更新。
 *
 * 这两类的字节计数器**恒为 0**（`StatsService` 不统计 UDP 出站与本机回环），
 * 所以不能与 `node` / `direct` / `block` 并列显示 `0 B` —— 那是把测量盲区
 * 画成了「没在用」。它们单独分组，并改用从访问日志解析出的**连接数**。
 */
export const INTERNAL_KINDS = new Set(["dns", "internal"]);

/**
 * 出口类别 → 颜色。
 *
 * 只有三种去向有颜色：经节点（蓝）/ 直连（绿）/ 已拦截（红）。
 * `dns` 与 `internal`（api）是**内部通道**，不是流向用户的去向，
 * 所以用中性灰 —— 早先没有为它们配色，fallback 成了蓝色，
 * 看起来像「另一条走节点的路」，那是误导。
 */
export const OUTBOUND_COLOR: Record<string, string> = {
  node: "#4f8ef7",
  direct: "#34d399",
  block: "#f87171",
  dns: "#64748b",
  internal: "#64748b",
};

/** 中性灰：内部通道（dns / api）的颜色。 */
export const NEUTRAL = "#64748b";

/**
 * 一条车道上的货车数量：按字节做对数映射到 [3, 8]。
 *
 * # 这里踩过一次坑，别再写 `1 << 40`
 *
 * JS 的位移是 **32 位取模**：`1 << 40 === 1 << 8 === 256`。于是上限
 * `hi = log10(256) = 2.41` 小于下限 `lo = log10(1 MiB) = 6.02`，比值恒为负、
 * 被 `Math.max(0, …)` 压成 0 —— **任何 ≥1 MiB 的流量都只画 3 辆**（与空车道一样），
 * 反过来 1 字节能画 8 辆。界面上「车辆数量由实测速率决定」因此是假的。
 * 用 `1024 ** n`（或 `2 ** 40`）才是真的 1 TiB。
 */
export function trucksOnLane(bytes: number): number {
  // **没流量就不画车。**
  //
  // 早先这里返回 3，注释写的是「空车道也画几辆，否则『量小』与『不通』
  // 看不出来」—— 但它恰恰把两者画成一样（都 3 辆），既没达成目的，又制造了
  // 「明明 0 B 却有车在跑」的假象。用户直接指出了这一点（http 入口 0 B 却有车）。
  //
  // 「量小」与「不通」本来就分得开：卡片的字节数分别显示 `↓1.2 MiB` 与
  // `↓0 B`，而 `traffic_ok === false` 时显示「流量不可用」。
  if (!Number.isFinite(bytes) || bytes <= 0) return 0;
  const lo = Math.log10(1024 ** 2); // 1 MiB
  const hi = Math.log10(1024 ** 4); // 1 TiB
  const t = Math.max(0, Math.min(1, (Math.log10(bytes) - lo) / (hi - lo)));
  return Math.round(3 + t * 5);
}

/** 一条路线的一段折线（屏幕坐标）。 */
export interface Seg {
  kind: "curve" | "line";
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** 曲线在 x 方向的中间控制点（"line" 忽略）。 */
  cx?: number;
  /** 这一段的颜色：扇出分支按目的地的出口类别着色。 */
  color?: string;
}

/** 一条完整的货运路线：入口 → 车道 → 扇出 → 出口。 */
export interface Route {
  /** 稳定身份 = 入口 tag。车靠它找到自己的路线，入口顺序变了也不会串。 */
  key: string;
  d: string;
  segs: Seg[];
  /** 这条路线属于哪个入口（货车数量按入口的字节数决定），下标对应 `inbound`。 */
  inlet: number;
  /**
   * 主干段（入口 → 分叉）。单连接高亮要拼「入口 → 分叉 → 该出口」这条**简单路径**，
   * 用 `routeToD([trunk, branch.fwd])` —— 而不是整条巡回路径。
   */
  trunk: Seg;
  /**
   * 每个出口分支：颜色（车**这一圈**就用它的颜色）+ 目的出口的 tag（单连接高亮靠它匹配）
   * + 去程那一段本身（拼高亮路径用）。
   *
   * ⚠️ 这里**没有「整圈累计比例」**（原来的 `startFrac`）：那个比例按「交错段序的前缀」
   * 算，而前缀里必然夹着回程段 —— 正是 task-37 修掉的错。
   */
  branches: { color: string; tag: string; fwd: Seg }[];
}

/** 一辆货车跨帧的全部状态。按**稳定身份**保存，不按数组下标。 */
export interface TruckState {
  /** 沿路径的**绝对路程（px）**，不是「占全程的比例」。 */
  dist: number;
  /**
   * 这一圈要送到**第几条分支**（按圈轮换）。一圈 = 主干 + 这一条分支；
   * 送完就 `(branch + 1) % 分支数` —— 每个出口都轮得到车，而且车不走回程段。
   */
  branch: number;
  /** 上一帧写进 `transform` 的屏幕坐标（首帧前为 NaN）。 */
  x: number;
  y: number;
  /** 上一帧该路线 `d` 的签名：变了说明几何变了，需要重锚。 */
  sig: string;
  /**
   * **正常行走**时的每帧步长（px）参考值，用来把「位置修正速度」限制在正常车速量级。
   * 只在没有被限速的帧上更新 —— 否则限速值会成为下一帧的参考，上限每帧 ×1.5 指数增长。
   */
  walkRef: number;
  /** 上一帧的填充色，避免每帧都写 DOM。 */
  color?: string;
}

/** 一个锚点，坐标相对 `.highway` 容器：`l`=卡片左缘、`r`=卡片右缘、`y`=垂直中心。 */
export interface Rel {
  l: number;
  r: number;
  y: number;
}

/** 一辆车走完全程需要的秒数（视觉节奏）。 */
export const TRAVEL_SECONDS = 7;

/**
 * 在 `path` 上找离点 `(px, py)` **最近**的弧长（px）。
 *
 * 用途：几何变化（卡片被撑宽、出口增减、窗口缩放）后，把货车的「上一帧屏幕点」
 * 投到新路径上作为新路程 —— 在「必须落到新路上」的前提下，这个落点位移最小。
 * 先粗采样 65 点，再在最优点两侧做黄金分割细化；只算距离，不依赖 `getPathSegAtLength`。
 */
export function nearestLength(path: SVGPathElement, total: number, px: number, py: number): number {
  const dist2 = (l: number): number => {
    const p = path.getPointAtLength(l);
    const dx = p.x - px;
    const dy = p.y - py;
    return dx * dx + dy * dy;
  };
  const N = 64;
  let best = 0;
  let bestD = Infinity;
  for (let i = 0; i <= N; i++) {
    const l = (i / N) * total;
    const d = dist2(l);
    if (d < bestD) {
      bestD = d;
      best = l;
    }
  }
  let lo = Math.max(0, best - total / N);
  let hi = Math.min(total, best + total / N);
  for (let it = 0; it < 12; it++) {
    const m1 = lo + (hi - lo) * 0.382;
    const m2 = lo + (hi - lo) * 0.618;
    if (dist2(m1) < dist2(m2)) hi = m2;
    else lo = m1;
  }
  return (lo + hi) / 2;
}

export function clampMid(x1: number, x2: number): number {
  const lo = Math.min(x1, x2);
  const hi = Math.max(x1, x2);
  return Math.min(hi, Math.max(lo, (x1 + x2) / 2));
}

/**
 * 把路线拼成**一条连续路径**：只有第一个节点用 `M`，其余都用 `C` / `L` 接上。
 *
 * 这一点很关键：早先每段各写一个 `M`，于是路径里有三段互不相连的子路径，
 * 而 `getPointAtLength` 是沿**一条**路径连续采样的 —— 货车会在段与段之间跳。
 * （表现就是车停在车道两端不动、中间的路程被跳过。）
 *
 * # 对调用者的硬要求：相邻两段必须**真的**首尾相接
 *
 * `C` 的起点是**上一段的终点**，参数里并没有「起点」—— 传进去的
 * `s.x1/s.y1` 只用来算控制点。所以若 `seg[i].x1/y1 !== seg[i-1].x2/y2`，
 * 合并路径会从上一段的终点「斜着」连到这一段的控制点上，几何与那些
 * **单独画出来的可见线**就不再是一回事：车会离线。
 * （实测踩过：把回程堆到最后，导致去第 2 个出口的曲线从第 1 个出口出发，
 * 46.7% 的行程离任何可见线 >1.5px、最大偏离 23.8px。）
 *
 * 直线段也用三次贝塞尔表示（控制点取在两端，退化成直线），
 * 这样整条路线是一条命令序列，长度与采样都可预期。
 */
export function routeToD(segs: Seg[]): string {
  if (segs.length === 0) return "";
  const parts = [`M ${segs[0]!.x1.toFixed(1)} ${segs[0]!.y1.toFixed(1)}`];
  for (const s of segs) {
    const cx = s.kind === "line" ? s.x1 : (s.cx ?? (s.x1 + s.x2) / 2);
    // 曲线的两个控制点：第一个贴着起点、第二个贴着终点，都取中间的 x
    parts.push(
      `C ${cx.toFixed(1)} ${s.y1.toFixed(1)}, ${cx.toFixed(1)} ${s.y2.toFixed(1)}, ${s.x2.toFixed(1)} ${s.y2.toFixed(1)}`,
    );
  }
  return parts.join(" ");
}

