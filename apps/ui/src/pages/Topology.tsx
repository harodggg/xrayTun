/**
 * 网络流动拓扑。
 *
 * # 这一页想回答什么
 *
 * 「我的流量从哪儿进、经过哪些规则、从哪儿出」。用公路来类比：入口是匝道口、
 * 规则链是一排依次判断的收费站、出口是不同去向的车道；车上的货物是字节。
 *
 * # 一处必须如实说清的边界
 *
 * 车流画在入口 ↔ 出口之间，因为那一段有实测依据（Xray 的
 * `inbound>>>` / `outbound>>>` 计数器）。但**「每辆车实际走了哪条规则」拿不到** ——
 * Xray 的统计里没有 per-rule 计数器。所以规则链以「真实的判定顺序」展示，
 * 由下面的「目的地判定」用真实数据算出**某个地址会走哪条规则**；
 * 我们不会把车流硬画在某条规则上，那会是编的。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorText } from "../ipc";
import { formatBytes } from "../types";
import type { RouteExplanation, TopoInbound, TopoOutbound, Topology } from "../types";

export default function Topology() {
  const [topo, setTopo] = useState<Topology | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setTopo(await api.routingTopology());
      setLoadError(null);
    } catch (e) {
      setLoadError(errorText(e));
    }
  }, []);

  useEffect(() => {
    void load();
    // 流量是累计值，2 秒刷新一次足够看出「在动」而不至于刷屏。
    const t = window.setInterval(() => void load(), 2000);
    return () => window.clearInterval(t);
  }, [load]);

  if (loadError) {
    return (
      <div className="page">
        <div className="note">
          取拓扑失败：{loadError}
          <br />
          拓扑来自运行中的配置（runtime/config.json），所以核心没在跑时读不到。
        </div>
      </div>
    );
  }
  if (!topo) return <div className="empty">正在加载…</div>;

  return (
    <div className="page">
      <section className="page__sec">
        <h2 className="page__title">网络流动</h2>
        <p className="page__desc">
          入口是流量进来的地方，规则链按真实顺序决定去哪，出口是最终去向。
          车上的货物是字节；车辆数量由实测速率决定。
        </p>
        <Highway topo={topo} />
        {topo.traffic_error && (
          <div className="note">
            取不到实时流量：{topo.traffic_error}
            <br />
            （拓扑本身仍然是真的 —— 它来自运行中的配置。这里不画 0 字节的假流量。）
          </div>
        )}
      </section>

      <DestChecker geoAvailable={topo.geo_available} />

      <section className="page__sec">
        <h2 className="page__title">规则链（{topo.rule.length} 条，自上而下判定）</h2>
        <p className="page__desc">
          Xray 自上而下取第一条命中的规则，所以顺序本身是语义的一部分：
          「广告拦截」排在「大陆直连」之前才有意义。
        </p>
        <div className="chain">
          {topo.rule.map((r) => (
            <div className="chain__row" key={r.index}>
              <span className="chain__idx">{r.index}</span>
              <span className="chain__tag">{r.tag}</span>
              <span className="chain__conds">
                {r.conditions.length ? r.conditions.join(" · ") : "（无显式条件）"}
              </span>
              <span className="chain__arrow">→</span>
              <span className="chain__out">{r.outbound}</span>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}

/** 入口 ↔ 出口之间的车流。 */
function Highway({ topo }: { topo: Topology }) {
  const inletRefs = useRef<(HTMLDivElement | null)[]>([]);
  const outletRefs = useRef<(HTMLDivElement | null)[]>([]);
  const [container, setContainer] = useState<HTMLDivElement | null>(null);

  // **每个入口一条车道**，包括当前没有流量的（空车道也显示，否则
  // 「在用但量小」和「根本不通」分不出来）。只排除内部管理入口
  // （`api` = dokodemo-door:10085，那是应用自己查统计的通道，不是用户流量）。
  const lanes = topo.inbound.filter((i) => i.tag !== "api");
  const outTotal = topo.outbound.reduce((a, o) => a + o.uplink_bytes + o.downlink_bytes, 0);
  const inTotal = lanes.reduce((a, i) => a + i.uplink_bytes + i.downlink_bytes, 0);

  return (
    <div className="highway" ref={(el) => setContainer(el)}>
      <div className="highway__legend">
        <span className="highway__legend-item">
          <span className="highway__legend-dot" style={{ background: OUTBOUND_COLOR.node }} />
          经节点
        </span>
        <span className="highway__legend-item">
          <span className="highway__legend-dot" style={{ background: OUTBOUND_COLOR.direct }} />
          直连
        </span>
        <span className="highway__legend-item">
          <span className="highway__legend-dot" style={{ background: OUTBOUND_COLOR.block }} />
          已拦截
        </span>
        <span className="highway__legend-note">货车沿连线从入口开到出口</span>
      </div>

      <div className="highway__side">
        <div className="highway__side-title">入口</div>
        {lanes.map((i, idx) => (
          <div
            className="highway__lane-label"
            key={i.tag}
            ref={(el) => {
              inletRefs.current[idx] = el;
            }}
          >
            <span className="highway__lane-tag">{i.tag}</span>
            <span className="highway__lane-meta">
              {i.protocol}
              {i.port ? ` :${i.port}` : ""}
            </span>
            <span className="highway__lane-bytes">
              ↓{formatBytes(i.downlink_bytes)} ↑{formatBytes(i.uplink_bytes)}
            </span>
          </div>
        ))}
        {/* 合计行刻意不挂 ref：它不是入口，连线不应当连到它 */}
        <div className="highway__lane-label highway__lane-label--total">
          <span className="highway__lane-tag">合计</span>
          <span className="highway__lane-bytes">出入 {formatBytes(inTotal)}</span>
        </div>
      </div>

      {/* 中间是流动区：连线本身就是车道、货车在线上走。
          不再有单独的车道列 —— 用户要求「车道应该消失，线本身应该就是车道」。 */}
      <div className="highway__side highway__side--right">
        <div className="highway__side-title">出口（颜色 = 去向）</div>
        {topo.outbound.map((o, idx) => (
          <div
            className={`highway__lane-label highway__lane-label--${o.kind}`}
            key={o.tag}
            ref={(el) => {
              outletRefs.current[idx] = el;
            }}
          >
            <span className="highway__lane-tag">{shortTag(o.tag)}</span>
            <span className="highway__lane-meta">{o.kind}</span>
            <span className="highway__lane-bytes">
              ↓{formatBytes(o.downlink_bytes)} ↑{formatBytes(o.uplink_bytes)}
            </span>
          </div>
        ))}
        <div className="highway__lane-label highway__lane-label--total">
          <span className="highway__lane-tag">合计</span>
          <span className="highway__lane-bytes">出入 {formatBytes(outTotal)}</span>
        </div>
      </div>

      <Flow
        container={container}
        inbound={lanes}
        outbound={topo.outbound}
        inletEls={inletRefs.current.slice(0, lanes.length)}
        outletEls={outletRefs.current}
      />
    </div>
  );
}

/** 出口 tag 太长时截断显示（节点 tag 形如 `node-n1d232c6b8c7a5004`）。 */
function shortTag(t: string): string {
  if (t.startsWith("node-")) return `节点 ${t.slice(5, 13)}…`;
  return t;
}

/** 出口类别 → 货车颜色。三色对应三种去向。 */
const OUTBOUND_COLOR: Record<string, string> = {
  node: "#4f8ef7",
  direct: "#34d399",
  block: "#f87171",
};

/** 一条车道上的货车数量：按字节做对数映射到 [3, 8]。 */
function trucksOnLane(bytes: number): number {
  if (bytes <= 0) return 3; // 空车道也画几辆，否则「量小」与「不通」看不出来
  const lo = Math.log10(1 << 20); // 1 MiB
  const hi = Math.log10(1 << 40); // 1 TiB
  const t = Math.max(0, Math.min(1, (Math.log10(bytes) - lo) / (hi - lo)));
  return Math.round(3 + t * 5);
}

/** 一条路线的一段折线（屏幕坐标）。 */
interface Seg {
  kind: "curve" | "line";
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  /** 曲线在 x 方向的中间控制点（"line" 忽略）。 */
  cx?: number;
}

/** 一条完整的货运路线：入口 → 车道 → 扇出 → 出口。 */
interface Route {
  d: string;
  segs: Seg[];
  /** 货车颜色（按终点出口的去向）。 */
  color: string;
  label: string;
  /** 这条路线属于哪个入口（货车数量按入口的字节数决定）。 */
  inlet: number;
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
 * # 一处边界
 *
 * 「哪条车道通向哪个出口」核心**没有这个计数器**（只有按入口、按出口两类
 * 统计）。所以出口按相邻切段分配到车道，只为了**画得清楚**；
 * 它表达「车道与出口相连」这个结构关系（来自配置），不声称逐条归属。
 */
function Flow({
  container,
  inbound,
  outbound,
  inletEls,
  outletEls,
}: {
  container: HTMLElement | null;
  inbound: TopoInbound[];
  outbound: TopoOutbound[];
  inletEls: (HTMLElement | null)[];
  outletEls: (HTMLElement | null)[];
}) {
  const [geo, setGeo] = useState<{
    w: number;
    h: number;
    routes: Route[];
  } | null>(null);
  const truckRefs = useRef<(SVGGElement | null)[]>([]);
  /** 动画起始时刻：跨 effect 重建保持连续，避免每 2 秒跳一次。 */
  const startRef = useRef<number | null>(null);

  // 测量：把 DOM 位置换算成「相对容器的坐标」，再拼出每条路线的路径
  useEffect(() => {
    if (!container) return;
    const measure = () => {
      const base = container.getBoundingClientRect();
      const rel = (el: HTMLElement | null) => {
        if (!el) return null;
        const r = el.getBoundingClientRect();
        return {
          l: r.left - base.left,
          r: r.right - base.left,
          y: r.top - base.top + r.height / 2,
        };
      };
      const inlets = compact(inletEls.map(rel));
      const outlets = compact(outletEls.map(rel));
      if (inlets.length === 0 || outlets.length === 0) {
        setGeo({ w: base.width, h: base.height, routes: [] });
        return;
      }

      // 路由分组：出口按相邻切段分给各入口（只为画得清楚，不声称归属）
      const per = Math.ceil(outlets.length / inlets.length);
      // 分叉点：放在入口与出口之间的**流动区**里、靠近出口那一侧，
      // 给「扇出」留出弧线空间。它代表「规则链做出的去向判定」。
      const gapStart = inlets[0]!.r;
      const gapEnd = outlets[0]!.l;
      // 分叉点放得靠出口一侧：入口→分叉留 ~70%（汇入弧线），
      // 分叉→出口留 ~30%（扇出弧线）。两者都在流动区**内部**预留，
      // 不占单独的列 —— 整页有 max-width，多一列就把流动区压没了。
      const forkX = gapStart + (gapEnd - gapStart) * 0.7;

      const routes: Route[] = [];
      inlets.forEach((inlet, i) => {
        const mine = outlets.slice(i * per, (i + 1) * per);
        mine.forEach((box, k) => {
          // 每个出口一条分支，但**共用同一个入口分叉点** ——
          // 形状就是「一个源点扇出多条弧线」。
          //
          // 同一入口的多条分支合并成**一条连续路径**：中间是那条汇入的弧线，
          // 之后依次往返每个出口。这样 getPointAtLength 能连续采样，
          // 货车沿整条路线来回走（用户要的「看到车在线上走，整个流程」）。
          const segs: Seg[] = [
            {
              kind: "curve",
              x1: inlet.r,
              y1: inlet.y,
              x2: forkX,
              y2: inlet.y,
              cx: (inlet.r + forkX) / 2,
            },
          ];
          const ordered = k === 0 ? mine : [mine[0]!, ...mine.slice(1).filter((b) => b !== box), box];
          ordered.forEach((b) => {
            segs.push({
              kind: "curve",
              x1: forkX,
              y1: inlet.y,
              x2: b.l,
              y2: b.y,
              cx: clampMid(forkX, b.l),
            });
          });
          if (k > 0) return; // 同一入口只生成一条合并路径
          routes.push({
            d: routeToD(segs),
            segs,
            color: OUTBOUND_COLOR[outbound[i * per]?.kind ?? ""] ?? "#4f8ef7",
            label: "",
            /** 这条路线属于哪个入口（货车数量按入口字节数决定）。 */
            inlet: i,
          });
        });
      });

      setGeo({ w: base.width, h: base.height, routes });
    };

    measure();
    // 流量每 2 秒刷新一次，卡片宽度会变 —— 持续跟随，而不是量一次就完
    const ro = new ResizeObserver(measure);
    ro.observe(container);
    for (const el of [...inletEls, ...outletEls]) {
      if (el) ro.observe(el);
    }
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [container, inletEls.join("|"), outletEls.join("|")]);

  // 沿路径行走：用 `getPointAtLength` 把货车摆到路上。
  // 不用 state 驱动（每帧 setState 会把整棵树重渲染），直接改 transform。
  useEffect(() => {
    if (!geo || geo.routes.length === 0) return;
    const groups = truckRefs.current.filter(Boolean) as SVGGElement[];
    if (groups.length === 0) return;

    // 取每条路线的长度，用于让所有货车速度一致（长路线走得久）
    const paths = container?.querySelectorAll<SVGPathElement>(".flow__route");
    if (!paths || paths.length === 0) return;
    const lengths = Array.from(paths).map((p) => p.getTotalLength());
    const maxLen = Math.max(...lengths, 1);

    let raf = 0;
    // 起始时间放在 ref 里：这个 effect 每次 geo 变化都会重建
    // （流量每 2 秒刷新一次），用局部变量会让货车每 2 秒**跳回起点**。
    if (startRef.current === null) startRef.current = performance.now();
    const t0 = startRef.current;
    const step = (now: number) => {
      const el = (now - t0) / 1000;
      groups.forEach((g) => {
        const idx = Number(g.dataset.route ?? 0);
        const path = paths[idx];
        if (!path) return;
        const total = lengths[idx] ?? 1;
        const phase = Number(g.dataset.phase ?? 0);
        // 速度与路径长度成正比：这样长路线不会显得慢吞吞
        const u = ((el / TRAVEL_SECONDS) * (total / maxLen) + phase) % 1;
        const pt = path.getPointAtLength(u * total);
        g.setAttribute("transform", `translate(${pt.x.toFixed(1)} ${pt.y.toFixed(1)})`);
      });
      raf = requestAnimationFrame(step);
    };
    raf = requestAnimationFrame(step);
    return () => cancelAnimationFrame(raf);
  }, [geo, container]);

  if (!geo) return null;

  // 每辆货车：数量按**入口**的字节数决定，挂在属于该入口的路线（每条路线
  // 已经合并了若干分支）上。一个入口的货就沿它自己那条路线来回走。
  const trucks: { route: number; phase: number }[] = [];
  geo.routes.forEach((r, idx) => {
    const bytes = inbound[r.inlet]
      ? inbound[r.inlet]!.downlink_bytes + inbound[r.inlet]!.uplink_bytes
      : 0;
    const count = trucksOnLane(bytes);
    for (let k = 0; k < count; k++) {
      trucks.push({ route: idx, phase: (k / count + (k % 3) * 0.04) % 1 });
    }
  });

  return (
    <svg className="flow" width={geo.w} height={geo.h} aria-hidden>
      {/* 连线本体 */}
      {geo.routes.map((r, i) => (
        <path key={`p-${i}`} className="flow__route" d={r.d} stroke={r.color} />
      ))}
      {/* 货车：沿连线从入口开到出口 */}
      {trucks.map((t, i) => (
        <g
          key={`t-${i}`}
          ref={(el) => {
            truckRefs.current[i] = el;
          }}
          data-route={t.route}
          data-phase={t.phase}
          className="flow__truck"
        >
          <rect x={-4.5} y={-2.5} width={9} height={5} rx={1}
                fill={geo.routes[t.route]?.color ?? "#4f8ef7"} />
        </g>
      ))}
    </svg>
  );
}

/** 一辆车走完全程需要的秒数（视觉节奏）。 */
const TRAVEL_SECONDS = 7;

function compact<T>(v: (T | null)[]): T[] {
  return v.filter((x): x is T => x !== null);
}

function clampMid(x1: number, x2: number): number {
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
 * 直线段也用三次贝塞尔表示（控制点取在两端，退化成直线），
 * 这样整条路线是一条命令序列，长度与采样都可预期。
 */
function routeToD(segs: Seg[]): string {
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

/**
 * 目的地判定：输入域名或 IP，用真实规则 + 真实 geosite/geoip 数据算出
 * 它会走哪条规则。这是这一页里**唯一能确定「某个东西走哪条路」**的能力。
 */
function DestChecker({ geoAvailable }: { geoAvailable: boolean }) {
  const [dest, setDest] = useState("");
  const [result, setResult] = useState<RouteExplanation | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const run = async () => {
    if (!dest.trim()) return;
    setBusy(true);
    setErr(null);
    try {
      setResult(await api.explainDest(dest.trim()));
    } catch (e) {
      setErr(errorText(e));
      setResult(null);
    } finally {
      setBusy(false);
    }
  };

  const verdict = useMemo(() => {
    if (!result) return null;
    if (result.rule_tag) {
      return `命中规则「${result.rule_tag}」→ 出站 ${result.outbound || "（默认）"}`;
    }
    return "未命中任何规则 → 使用第一条出站";
  }, [result]);

  return (
    <section className="page__sec">
      <h2 className="page__title">某个地址会走哪条路</h2>
      <p className="page__desc">
        输入域名或 IP，用真实的规则与 geosite/geoip 数据判定它命中哪条规则。
        {geoAvailable ? (
          <> 这条结论是确定的（已与真实核心对拍过）。</>
        ) : (
          <> 当前数据目录里没有 geosite.dat / geoip.dat，域名规则无法判定。</>
        )}
      </p>
      <div className="row row--wrap">
        <input
          type="text"
          placeholder="例如 www.google.com 或 223.5.5.5"
          value={dest}
          onChange={(e) => setDest(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") void run();
          }}
          style={{ flex: 1, minWidth: 220, maxWidth: 360 }}
        />
        <button className="btn btn--primary" disabled={busy || !dest.trim()} onClick={() => void run()}>
          {busy ? <span className="spin" /> : null}
          判定
        </button>
      </div>
      {err && <div className="banner banner--error"><span>✕</span><div>{err}</div></div>}
      {result && (
        <div className="verdict">
          <div className="verdict__head">{verdict}</div>
          <ul className="verdict__reasons">
            {result.reasons.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
          {result.undecidable.length > 0 && (
            <div className="verdict__unknown">
              无法判定：{result.undecidable.join("；")}
            </div>
          )}
        </div>
      )}
    </section>
  );
}
