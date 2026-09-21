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

import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { MutableRefObject } from "react";

import { api, errorText } from "../ipc";
import { formatBytes } from "../types";
import type {
  ConnectionRecord,
  RecentConnections,
  TopoInbound,
  TopoOutbound,
  Topology,
} from "../types";
import { matchConnectionToTopology, shortTag } from "../topology/connections";
import type { ConnectionMatch, TopologyTags } from "../topology/connections";
import {
  INTERNAL_KINDS,
  NEUTRAL,
  OUTBOUND_COLOR,
  TRAVEL_SECONDS,
  clampMid,
  nearestLength,
  routeToD,
  segApproxLen,
  segmentsOutboundLen,
  trucksOnLane,
} from "../topology/flowGeometry";
import type { Rel, Route, Seg, TruckState } from "../topology/flowGeometry";
import { RecentConnections as RecentConnectionsPanel } from "../topology/ConnectionsPanel";
import { DestChecker } from "../topology/DestChecker";

// `connections.test.ts`（**不在**本次重构的写入范围内）直接从本模块 import
// 这些具名导出，所以这里必须**原样转发** —— 不是「顺手加的 API」。
// 搬迁本身在 `./topology/connections.ts`。
export {
  CONNECTION_ROW_LIMIT,
  connectionKey,
  filterConnections,
  matchConnectionToTopology,
} from "../topology/connections";
export type { ConnectionFilter, ConnectionMatch, TopologyTags } from "../topology/connections";

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

  // ---- 最近连接（单连接可视化）-------------------------------------------
  //
  // 连接每秒可达数十条，但**不订阅逐条事件** —— 那会让 React 每秒重渲染几十次。
  // 与拓扑**共用同一个 2s interval**（见下面的 effect）：既够人眼读，也让渲染
  // 次数有上界。
  const [conns, setConns] = useState<RecentConnections | null>(null);
  const [connErr, setConnErr] = useState<string | null>(null);
  const [selected, setSelected] = useState<ConnectionRecord | null>(null);

  /** 取最近连接。走 `ipc.ts`（那里规定「invoke 只能在 ipc.ts」）。 */
  const loadConnections = useCallback(async (): Promise<RecentConnections> => {
    return api.recentConnections();
  }, []);

  const pollConnections = useCallback(async () => {
    try {
      setConns(await loadConnections());
      setConnErr(null);
    } catch (e) {
      setConnErr(errorText(e));
    }
  }, [loadConnections]);

  useEffect(() => {
    void load();
    void pollConnections();
    // **只有一个 interval**，同时刷拓扑与连接。
    //
    // 为什么刻意不注册第二个：本页的回归测试用 `window.setInterval` 桩**只保留
    // 最后一个回调**，多注册一个就会把拓扑刷新挤掉 —— 实测导致 4 条几何/数字
    // 回归测试变红（`refresh()` 调的不再是 `routingTopology`）。
    // 2s 对累计流量够用；连接按同一节奏取，渲染次数有上界。
    const t = window.setInterval(() => {
      void load();
      void pollConnections();
    }, 2000);
    return () => window.clearInterval(t);
  }, [load, pollConnections]);

  /**
   * 拓扑里的 tag 集合。连接的 `[入站 → 出站]` 就是用这对 tag 去匹配卡片的，
   * 所以流向图与内部通道要**分开列**（内部通道没有连线，匹配到也不能画线）。
   */
  const tags = useMemo<TopologyTags>(() => {
    const inb = topo?.inbound ?? [];
    const out = topo?.outbound ?? [];
    return {
      flowInlets: inb.filter((i) => i.tag !== "api").map((i) => i.tag),
      internalInlets: inb.filter((i) => i.tag === "api").map((i) => i.tag),
      flowOutlets: out.filter((o) => !INTERNAL_KINDS.has(o.kind)).map((o) => o.tag),
      internalOutlets: out.filter((o) => INTERNAL_KINDS.has(o.kind)).map((o) => o.tag),
    };
  }, [topo]);

  const selectedMatch = useMemo(
    () => (selected ? matchConnectionToTopology(selected, tags) : null),
    [selected, tags],
  );

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
          车上的货物是字节；车辆数量由累计流量决定（累计值只增不减，不代表当前速率）。
        </p>
        <MemoHighway topo={topo} match={selectedMatch} />
        {selectedMatch?.note && (
          <div className="note">
            单连接高亮：{selectedMatch.note}
          </div>
        )}
        {topo.traffic_error && (
          <div className="note">
            取不到实时流量：{topo.traffic_error}
            <br />
            （拓扑本身仍然是真的 —— 它来自运行中的配置。这里不画 0 字节的假流量。）
          </div>
        )}
        {/* 累计值跨核心重启被续接过：如实说明，**不做平滑掩盖**（后端刻意保留了这个信息） */}
        {topo.counter_resets > 0 && (
          <div className="note">
            核心重启过 {topo.counter_resets} 次，累计流量已续接（所以数字没有掉回 0）。
          </div>
        )}
      </section>

      <DestChecker geoAvailable={topo.geo_available} />

      <RecentConnectionsPanel
        payload={conns}
        error={connErr}
        tags={tags}
        selected={selected}
        onSelect={setSelected}
      />

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
function Highway({ topo, match }: { topo: Topology; match: ConnectionMatch | null }) {
  const inletRefs = useRef<(HTMLElement | null)[]>([]);
  const outletRefs = useRef<(HTMLElement | null)[]>([]);
  const [container, setContainer] = useState<HTMLDivElement | null>(null);
  /** 稳定的 callback ref：早先写成内联箭头，每次渲染都先 null 再 el，容器状态反复抖动。 */
  const containerRef = useCallback((el: HTMLDivElement | null) => setContainer(el), []);

  // **每个入口一条车道**，包括当前没有流量的（空车道也显示，否则
  // 「在用但量小」和「根本不通」分不出来）。只排除内部管理入口
  // （`api` = dokodemo-door:10085，那是应用自己查统计的通道，不是用户流量）。
  const lanes = topo.inbound.filter((i) => i.tag !== "api");
  /**
   * 这次流量是否可信。`traffic_ok === false` 时所有 `*_bytes` 都是**占位 0**
   * （见 `types.ts`），照单全收就会把「没查到」画成「0 B」——
   * 用户看到的「8.01 GiB → 0 → 8.01 GiB」正是这么来的。
   *
   * 两个信号都看：契约上是 `traffic_ok === (traffic_error === null)`，
   * 但任一个说「不可用」就按不可用处理，不赌后端不会只填其中一个。
   */
  const trafficOk = topo.traffic_ok !== false && !topo.traffic_error;
  // **内部通道与用户流向分开列**：`dns-out`（UDP）与 `api`（本机回环）的字节
  // 计数器恒为 0，那是 `StatsService` 的测量盲区而非事实（本机实测各有
  // 4769 / 5374 条连接）。把它们混在流向图里显示 `0 B`，会让人以为
  // 「这两个出口没在用」。所以它们单独一组，并改用**连接数**表示活跃度。
  const internal = topo.outbound.filter((o) => INTERNAL_KINDS.has(o.kind));
  const userOutbound = topo.outbound.filter((o) => !INTERNAL_KINDS.has(o.kind));
  // 合计只算用户流向，不把内部通道的占位 0 混进来
  const outTotal = userOutbound.reduce((a, o) => a + o.uplink_bytes + o.downlink_bytes, 0);
  const inTotal = lanes.reduce((a, i) => a + i.uplink_bytes + i.downlink_bytes, 0);

  return (
    <div
      className={`highway${match && (match.inlet || match.outlet) ? " highway--focused" : ""}`}
      ref={containerRef}
      data-focused={match && (match.inlet || match.outlet) ? "1" : undefined}
    >
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
        <span className="highway__legend-item">
          <span className="highway__legend-dot" style={{ background: NEUTRAL }} />
          内部通道
        </span>
        <span className="highway__legend-note">货车沿连线从入口开到出口</span>
      </div>

      <div className="highway__side">
        <div className="highway__side-title">入口</div>
        {lanes.map((i, idx) => (
          <div
            className={`highway__lane-label${match?.inlet === i.tag ? " highway__lane-label--match" : ""}`}
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
              {trafficOk ? `↓${formatBytes(i.downlink_bytes)} ↑${formatBytes(i.uplink_bytes)}` : "流量不可用"}
            </span>
          </div>
        ))}
        {/* 合计行刻意不挂 ref：它不是入口，连线不应当连到它 */}
        <div className="highway__lane-label highway__lane-label--total">
          <span className="highway__lane-tag">合计</span>
          {/* 查不到流量时**不把占位 0 算进合计**：合计会突然垮到 0 B，看着就像数据在跳 */}
          <span className="highway__lane-bytes">
            {trafficOk ? `出入 ${formatBytes(inTotal)}` : "流量不可用"}
          </span>
        </div>
      </div>

      {/* 中间是流动区：连线本身就是车道、货车在线上走。
          不再有单独的车道列 —— 用户要求「车道应该消失，线本身应该就是车道」。 */}
      <div className="highway__side highway__side--right">
        <div className="highway__side-title">出口（颜色 = 去向）</div>
        {userOutbound.map((o, idx) => (
          <div
            className={`highway__lane-label highway__lane-label--${o.kind}${match?.outlet === o.tag ? " highway__lane-label--match" : ""}`}
            key={o.tag}
            ref={(el) => {
              outletRefs.current[idx] = el;
            }}
          >
            <span className="highway__lane-tag">{shortTag(o.tag)}</span>
            <span className="highway__lane-meta">{o.kind}</span>
            <span className="highway__lane-bytes">
              {trafficOk ? `↓${formatBytes(o.downlink_bytes)} ↑${formatBytes(o.uplink_bytes)}` : "流量不可用"}
            </span>
          </div>
        ))}
        <div className="highway__lane-label highway__lane-label--total">
          <span className="highway__lane-tag">合计</span>
          <span className="highway__lane-bytes">
            {trafficOk ? `出入 ${formatBytes(outTotal)}` : "流量不可用"}
          </span>
        </div>

        {internal.length > 0 && (
          <div className="highway__internal">
            <div className="highway__side-title">内部通道（不计入合计）</div>
            {internal.map((o) => (
              <div
                className={`highway__lane-label highway__lane-label--internal${match?.outlet === o.tag ? " highway__lane-label--match" : ""}`}
                key={o.tag}
              >
                <span className="highway__lane-tag">{shortTag(o.tag)}</span>
                <span className="highway__lane-meta">{o.kind}</span>
                <span className="highway__lane-bytes">
                  {o.connections != null ? `${o.connections} 条连接` : "—"}
                </span>
              </div>
            ))}
            <div className="highway__note">
              这几个出口的<strong>字节数读不到</strong>：核心只在{" "}
              <span className="mono">StatsService</span> 里报流量，而它不统计 UDP 出站
              与本机回环。所以这里显示<strong>连接数</strong> —— 那是它们唯一可得的
              活跃度指标。（{internal.map((o) => o.tag).join("、")}）
            </div>
          </div>
        )}
      </div>

      <Flow
        container={container}
        inbound={lanes}
        outbound={userOutbound}
        inletRefs={inletRefs}
        outletRefs={outletRefs}
        trafficOk={trafficOk}
        highlight={match?.inlet && match.outlet ? { inlet: match.inlet, outlet: match.outlet } : null}
      />
    </div>
  );
}


/**
 * 车流图只在 `topo` 或高亮变化时重渲染。
 *
 * 连接列表每 1.5s 刷新一次，若把 `Highway` 一起拖进重渲染，React 每次都要
 * reconcile 整棵 SVG（几十条线 + 十几辆车）—— 动画虽然跑在 effect 里不受影响，
 * 但这份 reconcile 是纯浪费。`memo` 之后连接轮询与车流图解耦。
 */
const MemoHighway = memo(Highway);

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
function Flow({
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
        // 且首尾重合（闭环）—— 车走完一圈不会瞬移回起点。
        const segs: Seg[] = [trunk];
        const branches: Route["branches"] = [];
        let acc = segApproxLen(trunk);
        for (const p of pairs) {
          branches.push({ startFrac: 0, color: p.color, tag: p.tag, fwd: p.fwd }); // 比例稍后按整圈总长归一化
          segs.push(p.fwd, p.back);
          acc += segApproxLen(p.fwd) + segApproxLen(p.back);
        }
        segs.push(trunkBack);
        acc += segApproxLen(trunkBack);
        const lapApprox = acc || 1;
        let cum = segApproxLen(trunk);
        branches.forEach((b, k) => {
          b.startFrac = cum / lapApprox;
          cum += segApproxLen(pairs[k]!.fwd) + segApproxLen(pairs[k]!.back);
        });

        routes.push({
          // 稳定身份 = 入口 tag：入口顺序/数量变化时，车的身份不跟着漂。
          key: inbound[i]!.tag,
          d: routeToD(segs),
          segs,
          /** 这条路线属于哪个入口（货车数量按入口字节数决定）。 */
          inlet: i,
          trunk,
          branches,
          // 去程 = 主干 + 各分支（去/回成对插入，所以分支里偶数下标是去程）。
            outboundLen: (() => {
              const trunkLen = segApproxLen(segs[0]!);
              const total = segs.reduce((a, sg) => a + segApproxLen(sg), 0);
              const branchesLen = segs
                .slice(1)
                .filter((_, k) => k % 2 === 0)
                .reduce((a, sg) => a + segApproxLen(sg), 0);
              return segmentsOutboundLen(trunkLen, branchesLen, total);
            })(),
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

        let st = state.get(key);
        if (!st) {
          const phase = Number(g.dataset.phase ?? 0);
          st = {
            dist: (Number.isFinite(phase) ? phase : 0) * total,
            x: NaN,
            y: NaN,
            sig: sigs[idx]!,
            walkRef: 0,
          };
          state.set(key, st);
        }
        // 车的活动范围 = **去程长度**（回程段从不进入）。
        // 早先这里用整圈 `total`，车会走完回程 —— 屏幕上就是「倒着开」。
        const span = route.outboundLen > 0 ? route.outboundLen : total;
        // 速度按去程长度算：一轮 = 走完一次去程（TRAVEL_SECONDS 秒）
        const walk = (span / TRAVEL_SECONDS) * dt;

        // 几何变了（这条路的 `d` 变了）→ 用**上一帧的屏幕点**在新路径上取最近点重锚：
        // 在「必须落到新路上」的前提下，这个落点离原位置最近。
        if (st.sig !== sigs[idx]) {
          if (Number.isFinite(st.x) && Number.isFinite(st.y)) {
            st.dist = nearestLength(path, span, st.x, st.y);
          }
          st.sig = sigs[idx]!;
        }
        st.dist += walk;
        // **对去程长度取模 —— 车只在去程循环，永远不走回程段。**
        //
        // 这条是「小车怎么是来回的」的正解。早先路线是闭环、车走完整圈，回程在
        // 屏幕上就是倒着开。改成「走完整圈 + 把回程隐藏」也不行：实测隐藏占比
        // 92.8%，因为整圈里只有主干是重复的，去程只占约 11%。
        //
        // 对去程取模后，车始终走在看得见的路上；从最后一个出口回到入口的那一跳，
        // 语义上就是「这趟货送到了」。
        if (st.dist >= span) st.dist -= span * Math.floor(st.dist / span);

        const target = path.getPointAtLength(st.dist);
        let nx = target.x;
        let ny = target.y;
        let capped = false;
        if (Number.isFinite(st.x) && Number.isFinite(st.y)) {
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
        if (!capped) st.walkRef = stepLen; // 只有正常行走才更新参考步长
        st.x = nx;
        st.y = ny;
        g.setAttribute("transform", `translate(${nx.toFixed(1)} ${ny.toFixed(1)})`);

        // 车**不走回程**：进度对去程长度取模（见下面的 `st.dist` 处理），
        // 所以不存在「倒着开」。这里只是兜底清掉可能残留的隐藏状态。
        if (g.style.visibility === "hidden") g.style.visibility = "";

        // 颜色跟着**当前所在的分支**变：取整圈里**最靠后**那条已进入的分支，
        // 回程（回到分叉那段）沿用刚离开的那条分支的颜色，不闪回。
        const rect = g.querySelector("rect");
        if (rect) {
          const u = st.dist / total;
          let color = "";
          for (const b of route.branches) {
            if (u >= b.startFrac) color = b.color;
          }
          const fill = color || route.branches[0]?.color || NEUTRAL;
          if (fill !== st.color) {
            rect.setAttribute("fill", fill);
            st.color = fill;
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
        <g key={`p-${r.key}`}>
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
