/**
 * 拓扑页的**最近连接**面板：列表 + 过滤 + 详情。
 *
 * 从 `pages/Topology.tsx` 原样搬出（task-14 步骤 C，纯搬迁、行为不变）。
 * 两条边界随代码一起搬：只渲染最近 `CONNECTION_ROW_LIMIT` 行；
 * 不显示日志里没有的字段（没有每连接字节数、没有持续时间）。
 */

import { useMemo, useState } from "react";

import type { ConnectionRecord, RecentConnections } from "../types";

import {
  CONNECTION_ROW_LIMIT,
  connClock,
  connectionKey,
  filterConnections,
  matchConnectionToTopology,
  pairingPercent,
  shortTag,
} from "./connections";
import type { ConnectionFilter, ConnectionMatch, TopologyTags } from "./connections";

/**
 * 最近连接：列表 + 过滤 + 详情。
 *
 * # 两条必须守住的边界
 *
 * 1. **只渲染最近 `CONNECTION_ROW_LIMIT` 行**（连接可达每秒数十条），并且
 *    在标题里写出「共 N 条 / 已隐藏 M 条」—— 不偷偷截断。
 * 2. **不显示日志里没有的字段**：没有每连接字节数、没有持续时间。
 *    详情面板用灰字把这件事说明白，而不是留白让人以为「加载中」。
 */
export function RecentConnections({
  payload,
  error,
  tags,
  selected,
  onSelect,
}: {
  payload: RecentConnections | null;
  error: string | null;
  tags: TopologyTags;
  selected: ConnectionRecord | null;
  onSelect: (c: ConnectionRecord | null) => void;
}) {
  const [filter, setFilter] = useState<ConnectionFilter>({ inbound: "", outbound: "", query: "" });
  const items = payload?.items ?? [];
  const filtered = useMemo(() => filterConnections(items, filter), [items, filter]);
  const shown = filtered.slice(0, CONNECTION_ROW_LIMIT);
  const hidden = filtered.length - shown.length;
  const selectedKey = selected ? connectionKey(selected) : null;

  const inboundOptions = [...tags.flowInlets, ...tags.internalInlets];
  const outboundOptions = [...tags.flowOutlets, ...tags.internalOutlets];

  return (
    <section className="page__sec">
      <h2 className="page__title">最近连接</h2>
      <p className="page__desc">
        每条连接 = 核心访问日志里的一行 <span className="mono">accepted</span>。
        点一条，就会在上面那张车流图上高亮它走的那条路（入口 → 出站）。
        域名带 <span className="conn__star">*</span> 的是<strong>时序配对</strong>得到
        的近似值。
      </p>

      {error ? (
        <div className="note">
          取不到最近连接：{error}
          <br />
          （访问日志由核心写入 —— 核心没在跑时没有新行可读。）
        </div>
      ) : !payload ? (
        <div className="empty">正在读取访问日志…</div>
      ) : (
        <>
          <div className="conn-filter">
            <label className="conn-filter__field">
              <span>入站</span>
              <select
                value={filter.inbound}
                onChange={(e) => setFilter((f) => ({ ...f, inbound: e.target.value }))}
              >
                <option value="">全部</option>
                {inboundOptions.map((t) => (
                  <option key={t} value={t}>
                    {t}
                  </option>
                ))}
              </select>
            </label>
            <label className="conn-filter__field">
              <span>出站</span>
              <select
                value={filter.outbound}
                onChange={(e) => setFilter((f) => ({ ...f, outbound: e.target.value }))}
              >
                <option value="">全部</option>
                {outboundOptions.map((t) => (
                  <option key={t} value={t}>
                    {shortTag(t)}
                  </option>
                ))}
              </select>
            </label>
            <label className="conn-filter__field conn-filter__field--grow">
              <span>域名 / 目标</span>
              <input
                type="text"
                placeholder="例如 google 或 194.221.250.50"
                value={filter.query}
                onChange={(e) => setFilter((f) => ({ ...f, query: e.target.value }))}
              />
            </label>
            {(filter.inbound || filter.outbound || filter.query) && (
              <button
                className="btn btn--ghost"
                onClick={() => setFilter({ inbound: "", outbound: "", query: "" })}
              >
                清除过滤
              </button>
            )}
            {selected && (
              <button className="btn btn--ghost" onClick={() => onSelect(null)}>
                取消高亮
              </button>
            )}
          </div>
          {/* 过滤范围必须写出来：下面只过滤**已经取到的这批**，不是全量搜索。
              不说清楚会让人以为「搜遍了所有连接」——那是另一种「把局部当全部」的不诚实。
              另外**挤掉过才提挤掉**：一条都没挤掉过时说「更早的已被挤掉」没有指代对象，
              和「查不到就画 0 B」一样，是把不存在的事说成事实。 */}
          <div className="conn-scope">
            过滤范围：已取到的<strong>最近 {items.length} 条</strong>（不是全量搜索
            {payload.dropped > 0
              ? `；更早的连接已被环形缓冲挤掉，累计 ${payload.dropped} 条`
              : ""}
            ）
          </div>

          <div className="conn-summary">
            最近 {items.length} 条连接
            {filtered.length !== items.length && <> · 其中匹配 {filtered.length} 条</>}
            {hidden > 0 && <> · 列表只显示最近 {CONNECTION_ROW_LIMIT} 条（另有 {hidden} 条已隐藏）</>}
            {payload.pairing.accepted > 0 && (
              <>
                {" "}
                · 配到域名 {payload.pairing.paired} / {payload.pairing.accepted} 条（
                {pairingPercent(payload.pairing)}%）—— 配不到是正常的：IP 直连与内部通道
                本来就没有 sniffed 行
              </>
            )}
          </div>

          {shown.length === 0 ? (
            <div className="note">
              {items.length === 0
                ? "还没有连接记录（核心刚启动时正常）。"
                : `在已取到的最近 ${items.length} 条里没有匹配的连接（不是全量搜索）。`}
            </div>
          ) : (
            <div className="conn-list" role="list">
              {shown.map((c) => {
                const k = connectionKey(c);
                const on = k === selectedKey;
                return (
                  <button
                    type="button"
                    role="listitem"
                    key={k}
                    className={`conn-row${on ? " conn-row--on" : ""}`}
                    onClick={() => onSelect(on ? null : c)}
                  >
                    <span className="conn-row__time mono">{connClock(c)}</span>
                    <span className="conn-row__target">
                      {c.domain ? (
                        <>
                          {c.domain}
                          {c.domain_paired && (
                            <span className="conn__star" title="域名由日志两行时序配对得到，可能不准">
                              *
                            </span>
                          )}
                        </>
                      ) : (
                        <span className="conn-row__ip mono">
                          {c.target_host}
                          {c.target_port != null ? `:${c.target_port}` : ""}
                        </span>
                      )}
                    </span>
                    <span className="conn-row__route mono">
                      {c.inbound_tag} → {shortTag(c.outbound_tag)}
                    </span>
                  </button>
                );
              })}
            </div>
          )}

          {selected && (
            <ConnectionDetail
              c={selected}
              match={matchConnectionToTopology(selected, tags)}
              payload={payload}
            />
          )}
        </>
      )}
    </section>
  );
}

/** 选中连接的详情。**只显示日志里真的有的字段**，并把缺什么写清楚。 */
export function ConnectionDetail({
  c,
  match,
  payload,
}: {
  c: ConnectionRecord;
  match: ConnectionMatch;
  payload: RecentConnections | null;
}) {
  return (
    <div className="conn-detail">
      <div className="conn-detail__head">
        选中的连接
        {match.inlet && match.outlet && !match.note && (
          <span className="conn-detail__ok">已在车流图上高亮</span>
        )}
      </div>
      <dl className="conn-detail__grid">
        <dt>时间</dt>
        {/* 日志原样墙钟（毫秒精度）；`ts_ms` 是本进程收到该行的时刻 */}
        <dd className="mono">{c.ts_text}</dd>
        <dt>入站 → 出站</dt>
        <dd className="mono">
          {c.inbound_tag} → {c.outbound_tag}
        </dd>
        <dt>目标</dt>
        <dd className="mono">
          {c.target_host}
          {c.target_port != null ? `:${c.target_port}` : "（日志没给端口）"}
        </dd>
        <dt>协议</dt>
        <dd className="mono">{c.network}</dd>
        <dt>域名</dt>
        <dd>
          {c.domain ? (
            <>
              {c.domain}
              {c.domain_paired && <span className="conn__star">*</span>}
            </>
          ) : (
            <span className="conn-detail__muted">
              未配到（IP 直连 / 内部通道本来就没有 sniffed 行，这是正常态）
            </span>
          )}
        </dd>
        <dt>来源</dt>
        <dd className="mono">{c.from}</dd>
      </dl>

      {c.domain_paired && (
        <div className="conn-detail__caveat">
          <span className="conn__star">*</span> 域名是<strong>时序配对</strong>的结果：
          `sniffed` 与 `accepted` 是日志里两行，只能按时间就近配对
          {c.domain_pair_delta_us != null && <>（这条相差 {c.domain_pair_delta_us}µs）</>}，
          <strong>并发时可能对不上</strong>。
        </div>
      )}

      {match.note && <div className="note">{match.note}</div>}

      <div className="conn-detail__caveat">
        本视图<strong>没有</strong>这条连接的<strong>字节数</strong>与<strong>持续时间</strong>：
        Xray 的 <span className="mono">StatsService</span> 只有聚合计数器（没有 per-connection 流量），
        访问日志也只记建立（<span className="mono">accepted</span>）不记结束。所以这里不显示，
        也不推算。
      </div>

      {payload && payload.pairing.accepted > 0 && (
        <div className="conn-detail__meta">
          已观察 {payload.pairing.accepted} 条连接 · 配到域名 {payload.pairing.paired} 条（
          {pairingPercent(payload.pairing)}%）· 拒配（超时/乱序）{payload.pairing.rejected_stale} 次
          {payload.dropped > 0 && <> · 环形缓冲已挤掉 {payload.dropped} 条</>}
        </div>
      )}
    </div>
  );
}
