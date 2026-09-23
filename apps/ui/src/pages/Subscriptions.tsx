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
import { InlineConfirm } from "../InlineConfirm";
import { useStore } from "../store";
import { formatBytes, formatTimestamp, type Subscription } from "../types";

export default function Subscriptions() {
  const { snapshot, busy, run } = useStore();
  const subs = snapshot?.subscriptions ?? [];
  const nodes = snapshot?.nodes ?? [];

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
                /**
                 * task-120：**这是「删这个订阅会连带删掉几个节点」的唯一正确判据。**
                 *
                 * 原来用的是 `sub.node_count` —— 那个字段只在**订阅刷新成功**时写一次
                 * （`commands/nodes.rs:348`），手动删节点、导入去重（`:341`）都不会回写。
                 * 它是「上次解析出几条」，不是「现在真有几条」。删除确认语直接引用了它，
                 * 于是会告诉用户一个错的数量（本机预览数据里 sub-1 就写着 3，而实际只有 2 个）。
                 *
                 * 后端删除是按 source id 真删的（`nodes.rs:283-292`），所以这里也按同一个
                 * 判据数 —— 界面说的和即将发生的必须是同一件事。
                 */
                nodeCount={nodes.filter((n) => n.source.kind === "subscription" && n.source.id === sub.id).length}
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
  nodeCount,
  busy,
  onRefresh,
  onRemove,
}: {
  sub: Subscription;
  /** 现在真的有几个节点属于这个订阅（**不是** `sub.node_count`，见调用处注释）。 */
  nodeCount: number;
  busy: boolean;
  onRefresh: () => void;
  onRemove: () => void;
}) {
  // **`usage` 存在就该显示它**，与有没有配额无关。
  //
  // 后端把 `total == 0` 定义为「不限量」（见 `SubscriptionUsage::ratio`）。
  // 早先这里要求 `total > 0` 才认 `usage`，于是「不限量但有有效期」的订阅
  // **连到期时间都看不到** —— 而「什么时候到期」正是订阅最要紧的两条信息之一。
  // 现在 `usage` 只按存在与否判断，比例单独算，没有比例就不画条子。
  const usage = sub.usage;
  const used = usage ? usage.upload + usage.download : 0;
  const total = usage?.total ?? 0;
  const ratio = usage && total > 0 ? Math.min(1, used / total) : null;
  const tone = usageTone(ratio);
  const low = lowQuotaLabel(ratio);

  return (
    <div className={`list__row sub-row${sub.last_error ? " sub-row--error" : ""}`} style={{ cursor: "default" }}>
      <div className="list__main">
        <div className="sub-row__head">
          <span className="list__name">{sub.name}</span>
          <span className="list__meta">
            {nodeCount} 个节点 · 上次成功 {formatTimestamp(sub.last_updated)}
            {nodeCount === 0 && sub.last_updated === null && !sub.last_error ? "（还没拉到节点）" : ""}
          </span>
        </div>

        {usage && (
          <div className="usage">
            {/* 不限量（total == 0）没有比例可画，就不画条子 */}
            {ratio !== null && (
              <div className="usage__bar">
                <div
                  className={`usage__fill${tone ? ` usage__fill--${tone}` : ""}`}
                  style={{ width: `${(ratio * 100).toFixed(1)}%` }}
                />
              </div>
            )}
            <div className="usage__text">
              {usage.total === 0 ? (
                <span className="usage__pct">不限量 · 已用 {formatBytes(used)}</span>
              ) : (
                <span className={`usage__pct${tone ? ` usage__pct--${tone}` : ""}`}>
                  已用 {formatBytes(used)} / {formatBytes(total)}
                </span>
              )}
              {low && (
                <span className={`usage__warn${tone ? ` usage__warn--${tone}` : ""}`}>{low}</span>
              )}
              {usage.expire ? (
                <span className="usage__expire">到期 {formatTimestamp(usage.expire)}</span>
              ) : null}
            </div>
          </div>
        )}

        {sub.last_error && (
          <div className="sub-row__err">
            上次更新失败：{sub.last_error}
            <span className="sub-row__err-hint">
              {nodeCount > 0
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
        {/* 删订阅**连带删掉它带来的节点**（后端 `remove_subscription` → `nodes.retain(...)`），
            所以确认语里必须把「会删掉多少个节点」写出来 —— 只说「删除订阅」会让人以为
            只是少了一个订阅源。 */}
        <InlineConfirm
          label="删除"
          className="btn btn--ghost btn--danger"
          disabled={busy}
          title="删除该订阅"
          question={
            nodeCount > 0
              ? `删除订阅「${sub.name}」？会同时删除它带来的 ${nodeCount} 个节点，无法撤销。`
              : `删除订阅「${sub.name}」？此操作会写入配置文件，无法撤销。`
          }
          confirmLabel="确认删除"
          onConfirm={onRemove}
        />
      </div>
    </div>
  );
}

/**
 * 用量档位。`null`（没有配额或还没数据）与 `""` 都表示不强调。
 *
 * 抽成函数，而不是写成 `a ? b : c ? d : e`：这里有两个阈值加一个空值，
 * 串在一行里读不出「now at which tier」。顺带把边界集中到一处 ——
 * 早先标签用 `>=`、颜色用 `>`，恰好等于阈值时会出现「有文字但没颜色」。
 */
function usageTone(ratio: number | null): "" | "warn" | "danger" {
  if (ratio === null) return "";
  if (ratio >= DANGER_RATIO) return "danger";
  if (ratio >= WARN_RATIO) return "warn";
  return "";
}

/** 余量偏低时的结论文字；不低就没有。阈值与 [`usageTone`] 共用同一组常量。 */
function lowQuotaLabel(ratio: number | null): string | null {
  if (ratio === null) return null;
  if (ratio >= DANGER_RATIO) return "即将用尽";
  if (ratio >= WARN_RATIO) return "余量偏低";
  return null;
}
