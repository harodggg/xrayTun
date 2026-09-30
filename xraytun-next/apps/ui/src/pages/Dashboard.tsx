import { useState } from 'react';
import { ErrorBox } from '../components/ErrorBox';
import { StatusBadge } from '../components/StatusBadge';
import {
  MODE_TEXT,
  PHASE_TEXT,
  formatBytes,
  formatTime,
  hasCapability,
  toDisplayError,
  toDisplayErrorBody,
} from '../components/format';
import type { DisplayError } from '../components/format';
import { useDaemon } from '../store/daemon';
import type { RunMode } from '../transport/contract';

// Dashboard 只渲染 ConnectionView 里真实存在的字段。
// 每一个「未知」分支都显式写出来，而不是给个默认值糊过去。
export function Dashboard() {
  const { connection, nodes, settings, hello, client } = useDaemon();
  const [actionError, setActionError] = useState<DisplayError | null>(null);
  const [requestPending, setRequestPending] = useState(false);
  const [modeDraft, setModeDraft] = useState<RunMode | null>(null);

  const view = connection ?? null;
  const stage = view?.stage ?? null;
  const phase = view?.phase ?? null;
  const mode = view?.mode ?? null;
  const stats = view?.stats ?? null;
  const datapath = view?.datapath ?? null;
  const lastError = toDisplayErrorBody(view?.last_error);

  // node_id → 在真实节点目录里查名字；查不到就显示 id 本身，绝不编一个名字。
  const targetNodeId = view?.node_id ?? settings?.selected_node ?? null;
  const targetNode =
    targetNodeId == null ? null : (nodes.find((node) => node.id === targetNodeId) ?? null);
  const nodeLabel =
    targetNodeId == null ? '未选择' : targetNode != null ? targetNode.name : targetNodeId;

  const connecting = stage === 'connecting';
  const disconnecting = stage === 'disconnecting';
  const connected = stage === 'connected';
  const busy = connecting || disconnecting;

  // 入站模式的唯一开关依据是 capabilities 里的 'tun_mode'：
  // daemon 不宣告就只可能是 proxy；宣告了才有第二个选项。选 TUN 也可能失败，
  // 失败原样显示，不做任何模式回落。
  const canSelectTun = hasCapability(hello, 'tun_mode');
  const connectMode: RunMode = canSelectTun && modeDraft === 'tun' ? 'tun' : 'proxy';
  const canConnect = !busy && !connected && targetNodeId != null && !requestPending;
  const canDisconnect = connected && !requestPending;

  const connectBlockedReason = requestPending
    ? '请求已受理，等待事件返回终态'
    : connected
      ? '已连接'
      : disconnecting
        ? '断开进行中'
        : connecting
          ? `连接进行中：${phase != null ? PHASE_TEXT[phase] : '阶段未知'}`
          : targetNodeId == null
            ? '未选择节点'
            : null;

  const request = async (action: () => Promise<unknown>) => {
    setRequestPending(true);
    setActionError(null);
    try {
      await action();
    } catch (error) {
      // 失败原样展示：message 是 daemon 给这句话，不是我们编的话术。
      setActionError(toDisplayError(error));
    } finally {
      setRequestPending(false);
    }
  };

  const datapathKnown =
    datapath != null &&
    (datapath.pid != null || datapath.version != null || datapath.ready_at_ms != null);
  // 统计能力由 daemon 宣告；没有宣告就连统计卡都不渲染，
  // 免得用一张「未采样」的卡片假装我们有采样能力。
  const canShowStats = hasCapability(hello, 'stats');

  return (
    <div className="page dashboard" data-testid="page-dashboard">
      <h1 className="page__title">状态</h1>

      <section className="card">
        <div className="card__title">连接</div>
        <div className="card__body">
          <div className="row">
            <span className="row__label">状态</span>
            <span className="row__value">
              <StatusBadge stage={stage} phase={phase} />
            </span>
          </div>
          {view?.connected_since_ms != null && (
            <div className="row">
              <span className="row__label">连接建立于</span>
              <span className="row__value mono" data-testid="connected-since">
                {formatTime(view.connected_since_ms)}
              </span>
            </div>
          )}
          <div className="row">
            <span className="row__label">模式</span>
            <span className="row__value" data-testid="connection-mode">
              {mode != null ? MODE_TEXT[mode] : '未知'}
            </span>
          </div>
          <div className="row">
            <span className="row__label">当前节点</span>
            <span className="row__value">
              <span data-testid="current-node">{nodeLabel}</span>
              {targetNodeId != null && (
                <span className="muted mono">（id: {targetNodeId}）</span>
              )}
            </span>
          </div>
        </div>
        <div className="controls">
          {canSelectTun && (
            <label className="field">
              <span className="field__label">入站模式</span>
              <select
                className="select"
                data-testid="run-mode-select"
                value={connectMode}
                disabled={busy || connected || requestPending}
                onChange={(event) => setModeDraft(event.target.value as RunMode)}
              >
                <option value="proxy">代理模式（SOCKS 入站）</option>
                <option value="tun">TUN 模式</option>
              </select>
            </label>
          )}
          <button
            className="btn btn--primary"
            data-testid="connect-button"
            disabled={!canConnect}
            onClick={() => {
              if (targetNodeId == null) return;
              void request(() => client.connect(targetNodeId, connectMode));
            }}
          >
            连接
          </button>
          <button
            className="btn btn--danger"
            data-testid="disconnect-button"
            disabled={!canDisconnect}
            onClick={() => void request(() => client.disconnect())}
          >
            断开
          </button>
          {connectBlockedReason != null && (
            <span className="controls__note muted" data-testid="connect-blocked-reason">
              {connectBlockedReason}
            </span>
          )}
        </div>
        {actionError != null && <ErrorBox error={actionError} testId="connection-action-error" />}
      </section>

      {canShowStats && (
        <section className="card">
          <div className="card__title">真实流量</div>
          <div className="card__body">
            <div className={`stats${stats == null ? ' stats--unsampled' : ''}`}>
            <div className="stats__item">
              <span className="stats__label">上行</span>
              {stats == null ? (
                <span className="unknown" data-testid="stats-uplink">
                  未采样
                </span>
              ) : (
                <span className="stats__value mono" data-testid="stats-uplink">
                  {formatBytes(stats.uplink_bytes)}
                  <span className="muted">（{stats.uplink_bytes} 字节）</span>
                </span>
              )}
            </div>
            <div className="stats__item">
              <span className="stats__label">下行</span>
              {stats == null ? (
                <span className="unknown" data-testid="stats-downlink">
                  未采样
                </span>
              ) : (
                <span className="stats__value mono" data-testid="stats-downlink">
                  {formatBytes(stats.downlink_bytes)}
                  <span className="muted">（{stats.downlink_bytes} 字节）</span>
                </span>
              )}
            </div>
            <div className="stats__item">
              <span className="stats__label">采样时刻</span>
              {stats == null ? (
                <span className="unknown" data-testid="stats-sampled-at">
                  未采样
                </span>
              ) : (
                <span className="stats__sampled-at mono" data-testid="stats-sampled-at">
                  {formatTime(stats.sampled_at_ms)}
                </span>
              )}
            </div>
          </div>
          </div>
        </section>
      )}

      <section className="card">
        <div className="card__title">数据面</div>
        <div className="card__body">
          {datapathKnown ? (
            <>
              {datapath.pid != null && (
                <div className="row" data-testid="datapath-pid">
                  <span className="row__label">pid</span>
                  <span className="row__value mono">{datapath.pid}</span>
                </div>
              )}
              {datapath.version != null && (
                <div className="row" data-testid="datapath-version">
                  <span className="row__label">版本</span>
                  <span className="row__value mono">{datapath.version}</span>
                </div>
              )}
              {datapath.ready_at_ms != null && (
                <div className="row" data-testid="datapath-ready-at">
                  <span className="row__label">核心就绪时刻</span>
                  <span className="row__value mono">
                    {formatTime(datapath.ready_at_ms)}
                    <span className="muted">（{datapath.ready_at_ms} ms）</span>
                  </span>
                </div>
              )}
            </>
          ) : (
            <p className="unknown" data-testid="datapath-empty">
              未观测到数据面进程
            </p>
          )}
        </div>
      </section>

      {lastError != null && (
        <section className="card">
          <div className="card__title">最近一次失败</div>
          <div className="card__body">
            <ErrorBox error={lastError} testId="last-error" />
          </div>
        </section>
      )}
    </div>
  );
}
