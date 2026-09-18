/**
 * 订阅页。
 *
 * # 这一版改了什么
 *
 * 1. **「更新全部」从孤零零一个按钮变成列表区标题栏上的动作。** 原来它悬在
 *    列表上方单独一行，看不出作用对象是谁。
 * 2. **用量条成为卡片里最显眼的东西。** 订阅最要紧的信息是「还剩多少流量、
 *    什么时候到期」—— 原来它只是元信息里的一串数字 + 一条几乎看不见的细线。
 *    现在整条进度条 + 明确的比例文案，快用完时变色。
 * 3. **失败态给出可操作的信息。** 原来只把错误文字染红；现在把整行标成异常态，
 *    并说明「上次成功更新」是哪一次 —— 用户才能判断是否还有旧节点可用。
 * 4. 三张卡片 -> 分段内容 + 折叠的说明，与仪表盘/设置页同一套语言。
 */

import { useState } from "react";
import { api } from "../ipc";
import { useStore } from "../store";
import { formatBytes, formatTimestamp, type Subscription } from "../types";

export default function Subscriptions() {
  const { snapshot, busy, run } = useStore();
  const subs = snapshot?.subscriptions ?? [];

  const [name, setName] = useState("");
  const [url, setUrl] = useState("");

  const add = async () => {
    if (!url.trim()) return;
    const ok = await run("add-sub", () => api.addSubscription(name.trim(), url.trim()));
    if (ok) {
      setName("");
      setUrl("");
    }
  };

  return (
    <div className="page">
      <section className="page__sec">
        <h2 className="page__title">添加订阅</h2>
        <p className="page__desc">
          支持四种格式，会自动嗅探：机场通用的 base64 链接列表、明文链接列表、
          Clash / Mihomo 的 YAML、以及面板导出的 Xray JSON 配置。
          混入一两条不支持的链接（例如 hysteria2）不会导致整体失败，只会被跳过并记入日志。
        </p>
        <div className="row row--wrap">
          <input
            type="text"
            placeholder="备注名（可留空）"
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={{ width: 180 }}
          />
          <input
            type="text"
            placeholder="https://example.com/api/v1/client/subscribe?token=…"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            style={{ flex: 1, minWidth: 260 }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void add();
            }}
          />
          <button
            className="btn btn--primary"
            disabled={!url.trim() || busy !== null}
            onClick={() => void add()}
          >
            {busy === "add-sub" ? <span className="spin" /> : null}
            添加并拉取
          </button>
        </div>
        <p className="page__desc" style={{ marginTop: 8, marginBottom: 0 }}>
          订阅 URL 里通常带着账号令牌。日志里只会记录 host，完整 URL 不会出现在任何输出中。
        </p>
      </section>

      {subs.length === 0 ? (
        <div className="note">
          还没有订阅。添加后会自动拉取一次，并把节点合并进「节点」列表。
        </div>
      ) : (
        <section className="page__sec">
          <div className="page__bar">
            <h2 className="page__title" style={{ margin: 0 }}>
              {subs.length} 个订阅
            </h2>
            <span className="spacer" />
            <button
              className="btn btn--ghost"
              disabled={busy !== null}
              onClick={() => void run("refresh-all", () => api.refreshSubscriptions())}
            >
              {busy === "refresh-all" ? <span className="spin" /> : null}
              更新全部
            </button>
          </div>

          <div className="list">
            {subs.map((sub) => (
              <SubscriptionRow
                key={sub.id}
                sub={sub}
                busy={busy !== null}
                onRefresh={() => void run("refresh-one", () => api.refreshSubscriptions([sub.id]))}
                onRemove={() => void run("remove-sub", () => api.removeSubscription(sub.id))}
              />
            ))}
          </div>
        </section>
      )}
    </div>
  );
}

/** 用量比例超过它就变色：先提醒，再告警。 */
const WARN_RATIO = 0.7;
const DANGER_RATIO = 0.9;

function SubscriptionRow({
  sub,
  busy,
  onRefresh,
  onRemove,
}: {
  sub: Subscription;
  busy: boolean;
  onRefresh: () => void;
  onRemove: () => void;
}) {
  // `usage` 与 `total > 0` 一起收窄：两者任一不成立就没有比例可画，
  // 于是下面用到的 `usage` 一定是具体的值，不需要非空断言。
  const usage = sub.usage && sub.usage.total > 0 ? sub.usage : null;
  const used = usage ? usage.upload + usage.download : 0;
  const total = usage?.total ?? 0;
  const ratio = usage && total > 0 ? Math.min(1, used / total) : null;
  const tone = ratio === null ? "" : ratio > DANGER_RATIO ? "danger" : ratio > WARN_RATIO ? "warn" : "";

  return (
    <div className={`list__row sub-row${sub.last_error ? " sub-row--error" : ""}`} style={{ cursor: "default" }}>
      <div className="list__main">
        <div className="sub-row__head">
          <span className="list__name">{sub.name}</span>
          <span className="list__meta">
            {sub.node_count} 个节点 · 上次成功 {formatTimestamp(sub.last_updated)}
            {sub.node_count === 0 && !sub.last_error ? "（还没拉到节点）" : ""}
          </span>
        </div>

        {ratio !== null && (
          <div className="usage">
            <div className="usage__bar">
              <div
                className={`usage__fill${tone ? ` usage__fill--${tone}` : ""}`}
                style={{ width: `${(ratio * 100).toFixed(1)}%` }}
              />
            </div>
            <div className="usage__text">
              <span className={tone ? `usage__pct usage__pct--${tone}` : "usage__pct"}>
                已用 {formatBytes(used)} / {formatBytes(total)}
              </span>
              {ratio >= WARN_RATIO && (
                <span className={`usage__warn usage__warn--${tone}`}>
                  {ratio >= DANGER_RATIO ? "即将用尽" : "余量偏低"}
                </span>
              )}
              {usage?.expire ? <span className="usage__expire">到期 {formatTimestamp(usage.expire)}</span> : null}
            </div>
          </div>
        )}

        {sub.last_error && (
          <div className="sub-row__err">
            上次更新失败：{sub.last_error}
            <span className="sub-row__err-hint">
              {sub.node_count > 0
                ? "（已有节点仍然可用，可以稍后重试）"
                : "（拉取失败时不会移除既有节点，直接重试即可）"}
            </span>
          </div>
        )}
      </div>

      {/* 动作靠右下：卡片是纵向布局（名字/用量/错误各占一行），
          按钮贴右边缘和原设计一致 */}
      <div className="sub-row__actions">
        <button className="btn btn--ghost" disabled={busy} onClick={onRefresh}>
          更新
        </button>
        <button className="btn btn--ghost btn--danger" disabled={busy} onClick={onRemove}>
          删除
        </button>
      </div>
    </div>
  );
}
