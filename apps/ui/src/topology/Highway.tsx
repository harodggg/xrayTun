/**
 * 拓扑页的**入口 ↔ 出口车流区**：车道卡片、图例与流向图。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14 步骤 D，纯搬迁、行为不变）。
 * `MemoHighway` 是这里的对外入口（`memo` 让连接轮询与车流图解耦）。
 */

import { memo, useCallback, useRef, useState } from "react";

import { formatBytes } from "../types";
import type { Topology } from "../types";
import type { ConnectionMatch } from "./connections";
import { shortTag } from "./connections";
import { INTERNAL_KINDS, NEUTRAL, OUTBOUND_COLOR } from "./flowGeometry";
import { Flow } from "./Flow";

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
export const MemoHighway = memo(Highway);
