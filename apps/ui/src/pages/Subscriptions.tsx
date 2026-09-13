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
    <>
      <div className="card">
        <h2 className="card__title">添加订阅</h2>
        <p className="card__desc">
          支持四种格式，会自动嗅探：机场通用的 base64 链接列表、明文链接列表、
          Clash / Mihomo 的 YAML、以及面板导出的 Xray JSON 配置。
          订阅里混入一两条不支持的链接（例如 hysteria2）不会导致整体失败，只会被跳过并记入日志。
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
          />
          <button className="btn btn--primary" disabled={!url.trim() || busy !== null} onClick={() => void add()}>
            {busy === "add-sub" ? <span className="spin" /> : null}
            添加并拉取
          </button>
        </div>
        <div className="field__hint" style={{ marginTop: 8 }}>
          订阅 URL 里通常带着账号令牌。日志里只会记录 host，完整 URL 不会出现在任何输出中。
        </div>
      </div>

      {subs.length === 0 ? (
        <div className="empty">
          还没有订阅。
          <br />
          添加后会自动拉取一次，并把节点合并进「节点」列表。
        </div>
      ) : (
        <>
          <div className="row" style={{ marginBottom: 10 }}>
            <button
              className="btn"
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
        </>
      )}
    </>
  );
}

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
  const usage = sub.usage;
  const ratio = usage && usage.total > 0 ? Math.min(1, (usage.upload + usage.download) / usage.total) : null;

  return (
    <div className={`list__row${sub.last_error ? " is-disabled" : ""}`} style={{ cursor: "default" }}>
      <div className="list__main">
        <div className="list__name">{sub.name}</div>
        <div className="list__meta">
          {sub.node_count} 个节点 · 上次更新 {formatTimestamp(sub.last_updated)}
          {usage && usage.total > 0
            ? ` · 已用 ${formatBytes(usage.upload + usage.download)} / ${formatBytes(usage.total)}`
            : ""}
          {usage?.expire ? ` · 到期 ${formatTimestamp(usage.expire)}` : ""}
        </div>
        {ratio !== null && (
          <div
            style={{
              marginTop: 6,
              height: 3,
              borderRadius: 2,
              background: "var(--border)",
              overflow: "hidden",
              maxWidth: 320,
            }}
          >
            <div
              style={{
                width: `${(ratio * 100).toFixed(1)}%`,
                height: "100%",
                background: ratio > 0.9 ? "var(--danger)" : ratio > 0.7 ? "var(--warn)" : "var(--accent)",
              }}
            />
          </div>
        )}
        {sub.last_error && (
          <div className="list__meta" style={{ color: "var(--danger)", marginTop: 4 }}>
            上次失败：{sub.last_error}
          </div>
        )}
      </div>

      <button className="btn" disabled={busy} onClick={onRefresh}>
        更新
      </button>
      <button className="btn btn--ghost btn--danger" disabled={busy} onClick={onRemove}>
        删除
      </button>
    </div>
  );
}
