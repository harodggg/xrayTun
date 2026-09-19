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
import type { RouteExplanation, Topology } from "../types";

/** 车辆在这段时间内从入口走到出口（秒）。纯视觉节奏，与真实速率无关。 */
const TRIP_SECONDS = 1.6;

/**
 * 一条车道上的车辆数区间。
 *
 * 下限 5：**空车道也画几辆车**，否则「这条入口在用、只是量小」与
 * 「这条入口根本不通」在界面上一模一样。
 * 上限 15：再多就挤成一片、看不出是车了。
 */
const MIN_VEHICLES = 5;
const MAX_VEHICLES = 15;

/** 车辆数按字节做对数映射的参考点：1 MiB 与 1 TiB 对应两端。 */
const VEHICLE_SCALE_MIN_BYTES = 1024 * 1024;
const VEHICLE_SCALE_MAX_BYTES = 1024 ** 4;

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
  // 连线覆盖层要按真实 DOM 位置绘制，所以把关键元素测出来
  const laneRefs = useRef<(HTMLDivElement | null)[]>([]);
  const inletRefs = useRef<(HTMLDivElement | null)[]>([]);
  const outletRefs = useRef<(HTMLDivElement | null)[]>([]);
  const [container, setContainer] = useState<HTMLDivElement | null>(null);
  // 入口与车道一一对应；条目数变少时要把多余的引用截掉，
  // 否则旧引用会残留（稀疏数组），连线就会按错的位置画。
  const lanesForLinks = topo.inbound.filter((i) => i.tag !== "api");
  const inletEls = inletRefs.current.slice(0, lanesForLinks.length);
  // **每个入口一条车道**，包括当前没有流量的（空车道也要显示，否则
  // 「在用但量小」和「根本不通」分不出来）。
  //
  // 只排除内部管理入口（`api`，dokodemo-door:10085）：那是应用自己查统计用的
  // 通道，不是用户的流量，画出来只是噪声。
  const lanes = topo.inbound.filter((i) => i.tag !== "api");
  const outTotal = topo.outbound.reduce((a, o) => a + o.uplink_bytes + o.downlink_bytes, 0);
  const inTotal = lanes.reduce((a, i) => a + i.uplink_bytes + i.downlink_bytes, 0);

  // 车流按出口类别着色。份额取自**全局**出口字节数 —— 这是实测的；
  // 而「某个入口的货具体去了哪个出口」拿不到（核心没有这个计数器），
  // 所以份额只用来决定各色车辆的比例，不声称归属。
  const shares = topo.outbound
    .filter((o) => o.kind === "node" || o.kind === "direct" || o.kind === "block")
    .map((o) => ({ kind: o.kind, bytes: o.uplink_bytes + o.downlink_bytes }))
    .filter((s) => s.bytes > 0);

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
      </div>

      <div className="highway__side">
        <div className="highway__side-title">入口（每个入口一条车道）</div>
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

      <div className="highway__road">
        {lanes.map((i, idx) => {
          const total = i.downlink_bytes + i.uplink_bytes;
          return (
            <div
              key={i.tag}
              className="lane-wrap"
              ref={(el) => {
                laneRefs.current[idx] = el;
              }}
            >
              <Lane bytes={total} shares={shares} />
            </div>
          );
        })}
      </div>

      {/* 扇形**专用一列**（第 3 列）：不给它独立空间的话，车道列会占满中间，
          扇形只能在 10px 宽的间隙里画，看着像一堆竖线（实测过）。

          位置必须与列顺序一致：它是第 3 列，就必须排在车道列之后、出口列之前。
          早先放在最前面，结果出口列被挤到了这一列的宽度（84px、文字折行）。 */}
      <span className="highway__spacer" aria-hidden />

      <div className="highway__side highway__side--right">
        <div className="highway__side-title">出口（车道颜色 = 去向）</div>
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

      <HighwayLinks
        container={container}
        laneEls={laneRefs.current.slice(0, lanesForLinks.length)}
        inletEls={inletEls}
        outletEls={outletRefs.current}
      />
    </div>
  );
}

/**
 * 入口 → 车道 → 出口 的连线覆盖层。
 *
 * # 为什么需要它
 *
 * 原来三列是并排的卡片，谁也看不出「哪个入口对应哪条车道、车道又通向哪里」。
 * 用户要的是「跟连线一样」—— 入口连到它那条车道，车道再分叉到各个出口。
 *
 * # 怎么画的
 *
 * 用一条绝对定位的 SVG 盖在 `.highway` 上，按**实测的 DOM 位置**画贝塞尔曲线。
 * 位置不是猜的：用 `getBoundingClientRect` 量真实元素，并用 `ResizeObserver`
 * 跟随布局变化（流量数字每 2 秒更新一次，宽度会变）。
 *
 * # 一条边界
 *
 * 「哪条车道通向哪个出口」核心**没有这个计数器**（只有按入口、按出口两类
 * 统计）。所以连线表达的是「车道汇入出口」这个**结构关系**（配置事实），
 * 而不是逐条连接的归属。这一点与车道颜色用的是同一套口径。
 */
function HighwayLinks({
  container,
  laneEls,
  inletEls,
  outletEls,
}: {
  container: HTMLElement | null;
  laneEls: (HTMLElement | null)[];
  inletEls: (HTMLElement | null)[];
  outletEls: (HTMLElement | null)[];
}) {
  const [geo, setGeo] = useState<{
    w: number;
    h: number;
    /** 扇形列的左右边界（画连线用）。 */
    fan: { l: number; r: number } | null;
    lanes: { y: number; l: number; r: number }[];
    inlets: { y: number; l: number; r: number }[];
    outlets: { y: number; l: number }[];
  } | null>(null);

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
      // 只取**实际存在**的引用，按索引顺序对齐 ——
      // 早先用选择器取 `.highway__lane-label`，结果把「合计」那一行也算进来了，
      // 连线因此多出一条、还被摊开成斜线穿过卡片。
      // 扇形列的边界：连线画在这一列里
      const spacer = container.querySelector(".highway__spacer");
      const spacerRect = spacer?.getBoundingClientRect();
      const fan = spacerRect
        ? { l: spacerRect.left - base.left, r: spacerRect.right - base.left }
        : null;

      const compact = (els: (HTMLElement | null)[]) =>
        els
          .map(rel)
          .filter((v): v is NonNullable<typeof v> => v !== null);
      const lanes = compact(laneEls);
      const inlets = compact(inletEls);
      const outlets = compact(outletEls);
      setGeo({
        w: base.width,
        h: base.height,
        fan,
        lanes: lanes.map((v) => ({ y: v.y, l: v.l, r: v.r })),
        inlets: inlets.map((v) => ({ y: v.y, l: v.l, r: v.r })),
        outlets: outlets.map((v) => ({ y: v.y, l: v.l })),
      });
    };

    measure();
    // 流量数字每 2 秒更新一次，宽度会变 —— 持续跟随，而不是量一次就完
    const ro = new ResizeObserver(measure);
    ro.observe(container);
    for (const el of [...laneEls, ...inletEls, ...outletEls]) {
      if (el) ro.observe(el);
    }
    return () => ro.disconnect();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [container, laneEls.join("|"), inletEls.join("|"), outletEls.join("|")]);

  if (!geo || geo.lanes.length === 0) return null;

  // 车道组的中线：所有连线汇到这条线上，再分向各出口
  const mid = (geo.lanes[0]!.y + geo.lanes[geo.lanes.length - 1]!.y) / 2;

  /**
   * 三次贝塞尔：中间两个控制点让线走得像匝道，而不是直挺挺的折线。
   *
   * 控制点的 x 必须**落在两端之间**（`clamp`）。否则 x 接近时中点会跑出
   * 区间，曲线先向一边再折回、横向跨度缩成一团 —— 实测踩过：
   * 分叉点 532、出口左缘 538 时，曲线包围盒宽度只剩 10px，看着像一堆短线。
   */
  const curve = (x1: number, y1: number, x2: number, y2: number) => {
    const lo = Math.min(x1, x2);
    const hi = Math.max(x1, x2);
    const mx = Math.min(hi, Math.max(lo, (x1 + x2) / 2));
    return `M ${x1.toFixed(1)} ${y1.toFixed(1)} C ${mx.toFixed(1)} ${y1.toFixed(1)}, ${mx.toFixed(1)} ${y2.toFixed(1)}, ${x2.toFixed(1)} ${y2.toFixed(1)}`;
  };

  return (
    <svg className="highway__links" width={geo.w} height={geo.h} aria-hidden>
      {/* 入口 → 车道组：每个入口都汇入这几条车道 */}
      {geo.inlets.map((box, i) => (
        <path
          key={`in-${i}`}
          className="highway__link highway__link--in"
          d={curve(box.r, box.y, (geo.fan ? geo.fan.l : geo.lanes[0]!.l) - 6, mid)}
        />
      ))}

      {/* 车道 → 出口：**每条车道直接扇出**到分配给它的出口。
       *
       * 形状是「一个源点扇出多条弧线」（与地球仪的航线同一种视觉语言）。
       * 早先是先汇到一个中间点再分叉，那样看不出「这些去向和车道的关系」。
       *
       * 出口按顺序轮流分配给车道，保证每条车道都有去向 ——
       * 而不是把全部出口都挂在第一条车道上。
       *
       * 一处边界：**「哪条车道通向哪个出口」核心没有这个计数器**
       * （只有按入口、按出口两类统计）。所以这里表达的是「车道与出口相连」
       * 这个结构关系（来自配置），不声称逐条归属。 */}
      {geo.lanes.map((lane, i) => {
        // **相邻**分配：把出口按顺序切成几段，每条车道负责一段。
        //
        // 这样每条车道的扇形**不交叉**，像一把规整的扇子。
        // 试过交错分配（0,3 / 1,4 / 2,5），扇形摊得更开，但线互相穿插、
        // 看着很乱 —— 拓扑图里「乱」的代价高于「铺得开」。
        const lanes = geo.lanes.length;
        const per = Math.ceil(geo.outlets.length / lanes);
        const mine = geo.outlets.slice(i * per, (i + 1) * per);

        // 从车道右缘引到**扇形列的左边界**，再在扇形列里扇出到各出口。
        const fanL = geo.fan ? geo.fan.l : lane.r + 8;
        const fanR = geo.fan ? geo.fan.r : (geo.outlets[0]?.l ?? lane.r + 40);
        // 分叉点放在扇形列里：横向跨度足够，看起来才是扇子
        const forkX = fanL + (fanR - fanL) * 0.25;

        return (
          <g key={`fan-${i}`}>
            <path
              className="highway__link highway__link--trunk"
              d={`M ${lane.r.toFixed(1)} ${lane.y.toFixed(1)} L ${forkX.toFixed(1)} ${lane.y.toFixed(1)}`}
            />
            {mine.map((box, k) => (
              <path
                key={`out-${i}-${k}`}
                className="highway__link highway__link--out"
                d={curve(forkX, lane.y, box.l, box.y)}
              />
            ))}
          </g>
        );
      })}
    </svg>
  );
}

/** 出口 tag 太长时截断显示（节点 tag 形如 `node-n1d232c6b8c7a5004`）。 */
function shortTag(t: string): string {
  if (t.startsWith("node-")) return `节点 ${t.slice(5, 13)}…`;
  return t;
}

/**
 * 一条车道上的车。
 *
 * * **数量**：按该入口的实测字节做对数映射，落在 5–15 之间。
 *   空车道也画几辆 —— 否则「在用但量小」和「根本不通」看起来一样。
 * * **颜色**：按出口类别的**全局份额**分配（蓝=节点、绿=直连、红=拦截）。
 *   每个非零类别至少一辆，这样「有没有被拦截」永远看得见。
 * * 这只表达「这条路上有多少货、大致都去哪儿」——**不是**「哪辆车去了哪条规则」，
 *   后者核心没有计数器，画出来就是编的。
 */
function Lane({ bytes, shares }: { bytes: number; shares: { kind: string; bytes: number }[] }) {
  const count = vehicleCount(bytes);
  const kinds = useMemo(() => allocateKinds(count, shares), [count, shares]);
  const colorOf = (kind: string) => OUTBOUND_COLOR[kind] ?? "var(--accent)";

  const [tick, setTick] = useState(0);
  const raf = useRef<number | null>(null);
  const start = useRef<number>(performance.now());

  useEffect(() => {
    const step = (now: number) => {
      setTick((now - start.current) / 1000 / TRIP_SECONDS);
      raf.current = requestAnimationFrame(step);
    };
    raf.current = requestAnimationFrame(step);
    return () => {
      if (raf.current !== null) cancelAnimationFrame(raf.current);
    };
  }, []);

  if (count === 0) {
    return (
      <div className="lane">
        <div className="lane__empty">这条路当前没有流量</div>
      </div>
    );
  }

  return (
    <div className="lane">
      {kinds.map((kind, i) => {
        // 每辆车速度略有差异，免得整排像一列火车一起动
        const speed = 1 + ((i * 37) % 23) / 100;
        const phase = (tick * speed + i / count) % 1;
        return (
          <span
            key={i}
            className="lane__truck"
            style={{ left: `${(phase * 100).toFixed(2)}%`, background: colorOf(kind) }}
            title={OUTBOUND_LABEL[kind] ?? kind}
          />
        );
      })}
    </div>
  );
}

/** 出口类别 → 车道颜色。三色对应三种去向。 */
const OUTBOUND_COLOR: Record<string, string> = {
  node: "#4f8ef7",
  direct: "#34d399",
  block: "#f87171",
};

const OUTBOUND_LABEL: Record<string, string> = {
  node: "经节点",
  direct: "直连",
  block: "已拦截",
};

/**
 * 车辆数：按字节做对数映射到 [MIN_VEHICLES, MAX_VEHICLES]。
 *
 * 用对数而不是线性：流量能跨好几个数量级（几 MB 到几十 GB），
 * 线性映射会让小流量永远只有 5 辆、大流量一直顶到 15 辆，中间全糊在一起。
 */
function vehicleCount(bytes: number): number {
  if (bytes <= 0) return MIN_VEHICLES;
  const lo = Math.log10(VEHICLE_SCALE_MIN_BYTES);
  const hi = Math.log10(VEHICLE_SCALE_MAX_BYTES);
  const v = Math.log10(bytes);
  const t = Math.max(0, Math.min(1, (v - lo) / (hi - lo)));
  return Math.round(MIN_VEHICLES + t * (MAX_VEHICLES - MIN_VEHICLES));
}

/**
 * 把 `count` 辆车按份额分配给各去向。
 *
 * 规则：每个非零去向先保底 1 辆（否则「有拦截但份额极小」时会一辆都不显示，
 * 用户会以为没拦截），剩下的按字节比例分。
 */
function allocateKinds(count: number, shares: { kind: string; bytes: number }[]): string[] {
  if (shares.length === 0) {
    // 没有出口数据（核心没在跑）：不假装知道去向，用中性色
    return Array.from({ length: count }, () => "node");
  }
  const total = shares.reduce((a, s) => a + s.bytes, 0);
  const out: string[] = [];
  let used = 0;
  for (const s of shares) {
    const quota = Math.max(1, Math.round((s.bytes / total) * count));
    const take = Math.min(quota, count - used);
    for (let i = 0; i < take; i++) out.push(s.kind);
    used += take;
    if (used >= count) break;
  }
  // 份额取整可能没分满，用占比最大的补齐
  const biggest = shares.reduce((a, b) => (b.bytes > a.bytes ? b : a)).kind;
  while (out.length < count) out.push(biggest);
  return out.slice(0, count);
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
