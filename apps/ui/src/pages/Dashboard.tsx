import { api } from "../ipc";
import { useStore } from "../store";
import {
  formatBytes,
  formatRate,
  formatTimestamp,
  MODE_LABEL,
  nodeSummary,
  PRESET_LABEL,
} from "../types";

export default function Dashboard({ onNavigate }: { onNavigate: (view: string) => void }) {
  const { snapshot, busy, run, probing } = useStore();
  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { runtime, helper, core, nodes, settings, traffic, latency } = snapshot;
  const selected = nodes.find((n) => n.id === settings.selected_node) ?? null;
  const selectedLatency = selected ? latency[selected.id] : undefined;

  return (
    <>
      <Notices />

      <div className="grid-2" style={{ marginBottom: 14 }}>
        <div className="stat">
          <div className="stat__label">下行速率</div>
          <div className="stat__value">{formatRate(traffic.rx_rate)}</div>
          <div className="stat__sub">累计 {formatBytes(traffic.rx_bytes)}</div>
        </div>
        <div className="stat">
          <div className="stat__label">上行速率</div>
          <div className="stat__value">{formatRate(traffic.tx_rate)}</div>
          <div className="stat__sub">累计 {formatBytes(traffic.tx_bytes)}</div>
        </div>
        <div className="stat">
          <div className="stat__label">当前节点</div>
          <div className="stat__value" style={{ fontSize: 15 }}>
            {selected ? selected.name : "未选择"}
          </div>
          <div className="stat__sub">
            {selected ? nodeSummary(selected) : "请在「节点」页添加或选择"}
            {selectedLatency?.server_rtt_ms ? ` · ${selectedLatency.server_rtt_ms} ms` : ""}
          </div>
        </div>
        <div className="stat">
          <div className="stat__label">运行状态</div>
          <div className="stat__value" style={{ fontSize: 15 }}>
            {runtime.running ? `已连接（pid ${runtime.pid ?? "?"}）` : "未连接"}
          </div>
          <div className="stat__sub">
            {MODE_LABEL[settings.mode]} · {PRESET_LABEL[settings.routing_preset]}
            {runtime.tun_interface ? ` · ${runtime.tun_interface}` : ""}
          </div>
        </div>
      </div>

      <div className="card">
        <h2 className="card__title">快速操作</h2>
        <p className="card__desc">
          延迟探测会临时启动一个独立的核心实例，为每个节点单独开一个 SOCKS 端口来测量真实
          TTFB —— 不影响当前正在使用的连接。
        </p>
        <div className="row row--wrap">
          <button
            className="btn"
            disabled={probing || busy !== null || nodes.length === 0}
            onClick={() => void run("probe", () => api.testLatency())}
          >
            {probing ? <span className="spin" /> : null}
            测试全部节点延迟
          </button>
          <button
            className="btn"
            disabled={busy !== null}
            onClick={() => void run("refresh-subs", () => api.refreshSubscriptions())}
          >
            更新全部订阅
          </button>
          <button className="btn" onClick={() => void api.openDataDir()}>
            打开数据目录
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("logs")}>
            查看日志
          </button>
        </div>
      </div>

      <div className="card">
        <h2 className="card__title">环境自检</h2>
        <p className="card__desc">
          TUN 模式需要两个外部条件：一个新版 Xray 核心，以及已授权的特权 helper。
          只要有一项不满足，TUN 按钮就会失败并在这里显示原因。
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
                  <span className="badge badge--slow">
                    需 &gt;= {core.min_native_tun_version}
                  </span>
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
                <span className="badge badge--fast">已就绪</span> 版本{" "}
                {helper.version ?? "?"} · 协议 v{helper.protocol ?? "?"}
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

          <dt>运行中的配置</dt>
          <dd className="mono">
            {runtime.config_path ?? "（核心未运行）"}
          </dd>

          <dt>上次配置变更</dt>
          <dd>{formatTimestamp(runtime.started_at_unix)}</dd>
        </dl>
      </div>

      {runtime.last_error && (
        <div className="banner banner--error">
          <span>✕</span>
          <div>上次运行出错：{runtime.last_error}</div>
        </div>
      )}
    </>
  );
}

/** 顶部提示：把「需要用户做点什么」的情况明确说出来。 */
function Notices() {
  const { snapshot, busy, run } = useStore();
  if (!snapshot) return null;

  const { helper, core, notice, runtime } = snapshot;

  return (
    <>
      {runtime.running && !runtime.routes_committed && (
        <div className="banner banner--warn">
          <span>◐</span>
          <div>
            隧道已建立但<strong>默认路由尚未接管</strong>，流量还没有走代理。这通常意味着数据面还没就绪。
          </div>
        </div>
      )}

      {helper.stale_session && (
        <div className="banner banner--warn">
          <span>⚠︎</span>
          <div style={{ flex: 1 }}>
            检测到上次异常退出遗留的网络配置（会话 {helper.stale_session}）。
            这可能导致网络异常，建议立即回滚。
          </div>
          <button
            className="btn"
            disabled={busy !== null}
            onClick={() => void run("restore", () => api.restoreStale())}
          >
            立即修复
          </button>
        </div>
      )}

      {!core.path && (
        <div className="banner banner--error">
          <span>✕</span>
          <div>
            没有找到 Xray 核心。请把 <span className="mono">xray</span> 放到 app bundle 的
            Resources 目录，或在「设置 → 内核」里指定它的绝对路径。
          </div>
        </div>
      )}

      {core.path && !core.supports_native_tun && (
        <div className="banner banner--warn">
          <span>⚠︎</span>
          <div>
            当前核心的 TUN 实现不完整（需要 &gt;= {core.min_native_tun_version}）。
            macOS 上低于该版本时，核心只会创建网卡而不会配置地址与路由，TUN 模式将不可用。
          </div>
        </div>
      )}

      {note_visible(notice) && (
        <div className="banner banner--info">
          <span>ℹ︎</span>
          <div>{notice}</div>
        </div>
      )}
    </>
  );
}

function note_visible(notice: string | null): notice is string {
  return typeof notice === "string" && notice.length > 0;
}
