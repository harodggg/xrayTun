/**
 * 仪表盘。
 *
 * # 这一版的设计取舍
 *
 * 旧版把「现在通不通」拆在了三个地方：一张写着 pid 的卡片、顶栏的绿点、
 * 以及一条黄色横幅（「路由尚未接管」）。用户得自己把三处拼成一个结论 ——
 * 而规范第 2 条明确要求「连接成功必须精确表达」。
 *
 * 所以这里改成**一个状态区**：左侧圆点 + 状态词，右侧是唯一的主操作按钮，
 * 下面一行把「模式 · 路由 · 节点 · 延迟」串成一句话。那三个信息源合并成一处，
 * 中间态（隧道已建、路由未接管）作为状态词本身出现，而不是又一条横幅。
 *
 * 另外两处收敛：
 * * **横幅只留最要紧的一条。** 旧版最多会同时堆 5 条 + 底部一条 last_error，
 *   全部同权重，「流量还没走代理」和「helper 没装」看起来一样急。
 *   现在按严重度排序只显示第一条，其余折进一行「还有 N 条」。
 * * **流量与操作降低层级。** 速率是「看」的，不是「做」的，所以做成一行
 *   紧凑文本而不是两张占满的卡片；进程 pid、配置路径这类诊断信息收进
 *   「环境自检」折叠区 —— 出问题时才需要。
 */

import { useState } from "react";

import { api, recoveryView } from "../ipc";
import { useStore } from "../store";
import type { AppSnapshot } from "../types";
import {
  formatBytes,
  formatRate,
  formatTimestamp,
  latencyTier,
  MODE_LABEL,
  PRESET_LABEL,
} from "../types";

/** 一条横幅。`tone` 决定配色，`rank` 只用于排序（越小越急）。 */
interface Notice {
  key: string;
  tone: "error" | "warn" | "info";
  icon: string;
  text: React.ReactNode;
  rank: number;
  /** 需要用户立刻动手修的情况，附一个按钮。 */
  action?: { label: string; run: () => void };
}

export default function Dashboard({ onNavigate }: { onNavigate: (view: string) => void }) {
  const { snapshot, busy, run, probing, recovery } = useStore();
  const [showAllNotices, setShowAllNotices] = useState(false);
  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { runtime, helper, core, nodes, settings, traffic, latency } = snapshot;
  const selected = nodes.find((n) => n.id === settings.selected_node) ?? null;
  const selectedLatency = selected ? latency[selected.id] : undefined;
  const rtt = selectedLatency?.server_rtt_ms ?? null;
  const connected = runtime.running;
  /**
   * 自动恢复（task-22）：看门狗在自愈时**不能说成「未连接」** —— 那会让用户以为
   * 网络断了、去点「连接」，正好和看门狗抢。三态由结构化状态（`runtime.recovery`）
   * 驱动，**不解析 notice 文案**。
   */
  const rv = recoveryView(recovery, connected);

  // ---- 状态词：把「进程在跑」与「流量真的走代理了吗」合成一个结论 ----
  // 这是规范第 2 条要求的精确表达：中间态不能说成「已连接」。
  // 自动恢复的两态排在「未连接」**之前**，否则又会被盖成「未连接」。
  const state = !core.path
    ? { tone: "bad", dot: "", label: "未找到核心", sub: "缺少 Xray 可执行文件" }
    : rv.phase === "recovering"
      ? {
          tone: "warn",
          dot: "dot--warn",
          label: rv.text!,
          sub: "看门狗正在重建隧道，不需要手动点「连接」（点了会打断它）",
        }
      : rv.phase === "failed"
        ? {
            tone: "warn",
            dot: "dot--warn",
            label: "自动恢复失败",
            sub: "已退回直连：网络可用，但流量不再走代理 —— 可手动重连，或换一个节点",
          }
        : !connected
          ? { tone: "off", dot: "", label: "未连接", sub: "核心没有在运行" }
          : !runtime.routes_committed
            ? { tone: "warn", dot: "dot--warn", label: "隧道已建立", sub: "默认路由尚未接管，流量还没有走代理" }
            : { tone: "on", dot: "dot--on", label: "已连接", sub: null };

  const notices = collectNotices(snapshot, run, onNavigate);
  const [primary, ...rest] = notices;
  const visible = showAllNotices ? notices : primary ? [primary] : [];

  return (
    <div className="dash">
      {/* ---------------------------------------------------------- 状态区 */}
      <section className="dash__status">
        <div className={`dash__state dash__state--${state.tone}`}>
          <span className={`dot ${state.dot}`} />
          <span className="dash__state-label">{state.label}</span>
          {connected && selected && (
            <>
              <span className="dash__sep">·</span>
              <span className="dash__state-node">{selected.name}</span>
              {rtt !== null && (
                <span className={`badge badge--${latencyTier(rtt)}`}>{rtt} ms</span>
              )}
            </>
          )}
        </div>

        <div className="dash__meta">
          {MODE_LABEL[settings.mode]}
          <span className="dash__sep">·</span>
          {PRESET_LABEL[settings.routing_preset]}
          {runtime.tun_interface && (
            <>
              <span className="dash__sep">·</span>
              {runtime.tun_interface}
            </>
          )}
          {connected && runtime.started_at_unix && (
            <>
              <span className="dash__sep">·</span>
              已运行 {elapsed(runtime.started_at_unix)}
            </>
          )}
        </div>

        {state.sub && <div className="dash__state-sub">{state.sub}</div>}

        <div className="dash__actions">
          <button
            className={`btn ${connected ? "btn--danger" : "btn--primary"}`}
            disabled={busy !== null || settings.mode === "direct" || !core.path}
            onClick={() => void run(connected ? "stop" : "start", connected ? api.stop : api.start)}
            title={settings.mode === "direct" ? "直连模式下无需启动核心" : undefined}
          >
            {busy === "start" || busy === "stop" ? <span className="spin" /> : null}
            {connected ? "断开" : "连接"}
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("nodes")}>
            {connected ? "切换节点" : "选择节点"}
          </button>
          <button
            className="btn btn--ghost"
            disabled={probing || busy !== null || nodes.length === 0}
            onClick={() => void run("probe", () => api.testLatency())}
          >
            {probing ? <span className="spin" /> : null}
            测试延迟
          </button>
        </div>

        {/* 直连模式下连接按钮是禁用的 —— 说明原因，而不是让用户猜 */}
        {settings.mode === "direct" && (
          <div className="dash__hint">
            当前是<strong>直连</strong>模式，核心不会接管任何流量。要使用代理请先切换模式。
          </div>
        )}
      </section>

      {/* ---------------------------------------------------------- 流量 */}
      <section className="dash__traffic">
        <div className="dash__metric">
          <span className="dash__metric-arrow">↓</span>
          <span className="dash__metric-rate">{formatRate(traffic.rx_rate)}</span>
          <span className="dash__metric-total">累计 {formatBytes(traffic.rx_bytes)}</span>
        </div>
        <div className="dash__metric">
          <span className="dash__metric-arrow">↑</span>
          <span className="dash__metric-rate">{formatRate(traffic.tx_rate)}</span>
          <span className="dash__metric-total">累计 {formatBytes(traffic.tx_bytes)}</span>
        </div>
      </section>

      {/* ---------------------------------------------------------- 提示 */}
      {visible.map((n) => (
        <div key={n.key} className={`banner banner--${n.tone} dash__banner`}>
          <span>{n.icon}</span>
          <div style={{ flex: 1 }}>{n.text}</div>
          {n.action && (
            <button className="btn" onClick={n.action.run}>
              {n.action.label}
            </button>
          )}
        </div>
      ))}
      {rest.length > 0 && !showAllNotices && (
        <button className="dash__more" onClick={() => setShowAllNotices(true)}>
          还有 {rest.length} 条提示
        </button>
      )}

      {/* ------------------------------------------------- 诊断（默认折叠） */}
      <details className="dash__details">
        <summary>环境自检与诊断</summary>
        <p className="card__desc">
          TUN 模式需要两个外部条件：一个新版 Xray 核心，以及已授权的特权 helper。
          任何一项不满足，TUN 都会失败 —— 这里显示的就是它们当前的真实状态。
        </p>
        <dl className="kv">
          <dt>Xray 核心</dt>
          <dd>
            {core.path ? (
              <>
                <span className="mono">{core.path}</span>
                <br />
                {core.version ?? "（无法读取版本）"}{" "}
                {core.supports_native_tun ? (
                  <span className="badge badge--fast">支持原生 TUN</span>
                ) : (
                  <span className="badge badge--slow">需 &gt;= {core.min_native_tun_version}</span>
                )}
              </>
            ) : (
              <span style={{ color: "var(--danger)" }}>{core.error ?? "未找到"}</span>
            )}
          </dd>

          <dt>特权 helper</dt>
          <dd>
            {helper.reachable ? (
              <>
                <span className="badge badge--fast">已就绪</span> 版本 {helper.version ?? "?"} · 协议 v
                {helper.protocol ?? "?"}
                {helper.tun_active ? " · 有活跃隧道" : ""}
              </>
            ) : helper.socket_present ? (
              <span style={{ color: "var(--warn)" }}>
                已安装但无法连接{helper.error ? `：${helper.error}` : ""}
              </span>
            ) : (
              <span style={{ color: "var(--text-dim)" }}>
                未安装（仅影响 TUN 模式，系统代理模式不受影响）
              </span>
            )}
          </dd>

          <dt>核心进程</dt>
          <dd>{connected ? `运行中（pid ${runtime.pid ?? "?"}）` : "未运行"}</dd>

          <dt>运行中的配置</dt>
          <dd className="mono">{runtime.config_path ?? "（核心未运行）"}</dd>

          <dt>上次配置变更</dt>
          <dd>{formatTimestamp(runtime.started_at_unix)}</dd>
        </dl>

        <div className="row row--wrap" style={{ marginTop: 12 }}>
          <button
            className="btn btn--ghost"
            disabled={busy !== null}
            onClick={() => void run("refresh-subs", () => api.refreshSubscriptions())}
          >
            更新全部订阅
          </button>
          <button className="btn btn--ghost" onClick={() => void api.openDataDir()}>
            打开数据目录
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("logs")}>
            查看日志
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("settings")}>
            全部设置
          </button>
        </div>
      </details>
    </div>
  );
}

/**
 * 把「需要用户知道的事」按严重度收集起来。
 *
 * 旧版把这些写成 5 个平铺的 `{cond && <Banner/>}`，顺序是代码顺序而不是
 * 紧急程度，于是「流量还没走代理」可能排在「helper 没装」下面。
 * 收集后由调用方只显示最急的一条 —— 需要看全时也仍然拿得到。
 */
function collectNotices(
  snapshot: AppSnapshot,
  run: ReturnType<typeof useStore>["run"],
  onNavigate: (view: string) => void,
): Notice[] {
  const { core, helper, notice, runtime } = snapshot;
  const out: Notice[] = [];

  // rank 越小越急：先「现在就是坏的」，再「需要修」，最后「仅供参考」。
  if (runtime.last_error) {
    out.push({
      key: "last-error",
      tone: "error",
      icon: "✕",
      rank: 0,
      text: <>上次运行出错：{runtime.last_error}</>,
    });
  }

  if (!core.path) {
    out.push({
      key: "no-core",
      tone: "error",
      icon: "✕",
      rank: 1,
      text: (
        <>
          没有找到 Xray 核心。请把 <span className="mono">xray</span> 放到 app bundle 的 Resources
          目录，或在设置里指定它的绝对路径。
        </>
      ),
    });
  }

  if (helper.stale_session) {
    out.push({
      key: "stale",
      tone: "warn",
      icon: "⚠︎",
      rank: 2,
      text: (
        <>
          检测到上次异常退出遗留的网络配置（会话 {helper.stale_session}）。这可能导致网络异常，
          建议立即回滚。
        </>
      ),
      action: {
        label: "立即修复",
        run: () => void run("restore", () => api.restoreStale()),
      },
    });
  }

  if (core.path && !core.supports_native_tun) {
    out.push({
      key: "old-core",
      tone: "warn",
      icon: "⚠︎",
      rank: 3,
      text: (
        <>
          当前核心的 TUN 实现不完整（需要 &gt;= {core.min_native_tun_version}）。
          低于该版本时核心只会创建网卡，不会配置地址与路由，TUN 模式不可用。
        </>
      ),
    });
  }

  if (notice) {
    out.push({
      key: "notice",
      tone: "info",
      icon: "ℹ︎",
      rank: 4,
      text: <>{notice}</>,
      action: {
        label: "去处理",
        // helper 未装是最常见的来源，直接带去设置页比让用户自己找路更省事
        run: () => onNavigate(helper.socket_present ? "logs" : "settings"),
      },
    });
  }

  return out.sort((a, b) => a.rank - b.rank);
}

/** 「已运行 12 分钟」这种相对时间。只用于状态区，精度到分钟就够。 */
function elapsed(startedAtUnix: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - startedAtUnix);
  if (secs < 60) return `${secs} 秒`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟`;
  const hours = Math.floor(secs / 3600);
  if (hours < 24) return `${hours} 小时 ${Math.floor((secs % 3600) / 60)} 分钟`;
  return `${Math.floor(hours / 24)} 天`;
}
