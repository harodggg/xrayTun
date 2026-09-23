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

import { useCallback, useEffect, useMemo, useState } from "react";

import { api, errorText } from "../ipc";
import type { ConnectionRecord, RecentConnections, Topology } from "../types";
import { matchConnectionToTopology } from "../topology/connections";
import type { TopologyTags } from "../topology/connections";
import { INTERNAL_KINDS } from "../topology/flowGeometry";
import { RecentConnections as RecentConnectionsPanel } from "../topology/ConnectionsPanel";
import { DestChecker } from "../topology/DestChecker";
import { MemoHighway } from "../topology/Highway";

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
          车上的货物是字节；车辆数量由累计流量决定（<strong>本次会话内</strong>
          累计值只增不减，不代表当前速率）。
        </p>
        {/*
          task-126：原来的写法是**无条件**的「累计值只增不减」。这句话只在**本次 App
          运行期间**成立 —— 续接用的单调化基数是一个**进程内的 static**
          （`apps/desktop/src/commands/topology.rs:225` 的 `TRAFFIC_COUNTERS`，
          `OnceLock<Mutex<MonotonicCounters>>`），进程结束就没了；而
          `counter_resets` 报的是 `MonotonicCounters::max_resets()`，也就是
          **本进程内观察到的**归零次数。
          ⇒ 重启 App 之后第一次读到的是核心当时的**原始**计数：核心如果跟着重启
          （正常退出路径就是这样），数字会掉回 ~0，而这一次掉回**不会**出现在
          `counter_resets` 里（那时新进程刚开始观察）。原来的说法把这种情形说成了
          「不会发生」。所以这里把适用范围写出来，不猜、也不平滑掩盖。
        */}
        <p className="page__desc">
          ⚠︎ 这条「只增不减」的保证<strong>只活在本次 App 运行期间</strong>：重启 App 后第一次读到的是
          核心当时的原始计数（核心也一起重启了就会看到数字掉回 0），而下面那条
          「核心重启过 N 次」只统计<strong>本次运行期间</strong>观察到的归零。两件事都不隐瞒。
        </p>
        <MemoHighway topo={topo} match={selectedMatch} />
        {selectedMatch?.note && (
          <div className="note">
            {/* task-126：内部通道**画不出线**，前缀却写着「高亮」—— 一句话自相矛盾。
                前缀跟着 `inFlow`（那个字段就是「能不能画线」）走。 */}
            单连接{selectedMatch.inFlow ? "高亮" : ""}：{selectedMatch.note}
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
