import { useEffect, useRef, useState } from 'react';
import { ErrorBox } from '../components/ErrorBox';
import {
  MODE_TEXT,
  elapsedText,
  formatBytes,
  formatRate,
  formatTime,
  hasCapability,
  toDisplayError,
} from '../components/format';
import type { DisplayError } from '../components/format';
import { collectBanners } from '../components/notices';
import { startButtonView } from '../components/startButton';
import { DASH_TONE_CLASS, DOT_TONE_CLASS, appStatus } from '../components/topbarStatus';
import { INITIAL_RATE, deriveRate, sampleFrom } from '../components/trafficRate';
import type { RateState } from '../components/trafficRate';
import { useDaemon } from '../store/daemon';

/** 延迟分档：只影响配色；文字始终是真实毫秒数（拿不到就整块不渲染，不显示 0 ms）。 */
function ttfbTier(ms: number): 'fast' | 'ok' | 'slow' {
  if (ms < 150) return 'fast';
  if (ms < 400) return 'ok';
  return 'slow';
}

/** 仪表板能把用户带到哪一页。与 App.tsx 的 PageId 对齐。 */
export type DashboardTarget = 'nodes' | 'logs' | 'settings';

/**
 * 仪表板：按老版 XrayTun（v0.9.2）的形状重建，只绑定 v1 契约里真实存在的字段。
 *
 * 这一版的三条硬规则：
 *  1. **大启动按钮是唯一的主控**（全屏只有这一处启动/停止）；
 *  2. **只有一种模式：TUN**；界面没有任何模式选择器，`Connect` 恒发 `tun`；
 *  3. 拿不到的数据就**不显示**（老版的 core/helper/更新检查/分流预设在这里没有数据源），
 *     速率没有两个样本时显示「未采样」，绝不显示 0（I3）。
 */
export function Dashboard({ onNavigate }: { onNavigate: (target: DashboardTarget) => void }) {
  const { connection, nodes, settings, hello, notices, client } = useDaemon();
  const [requestPending, setRequestPending] = useState(false);
  const [actionError, setActionError] = useState<DisplayError | null>(null);
  const [probing, setProbing] = useState(false);
  const [showAllBanners, setShowAllBanners] = useState(false);
  const [rates, setRates] = useState<{ uplink: number | null; downlink: number | null }>({
    uplink: null,
    downlink: null,
  });
  const rateRef = useRef<{ uplink: RateState; downlink: RateState }>({
    uplink: INITIAL_RATE,
    downlink: INITIAL_RATE,
  });

  const loaded = connection !== null;
  const stage = connection?.stage ?? null;
  const phase = connection?.phase;
  const stats = connection?.stats ?? null;
  const datapath = connection?.datapath ?? null;
  const lastError = connection?.last_error ?? null;
  const connected = stage === 'connected';
  const hasTun = hasCapability(hello, 'tun_mode');
  const canShowStats = hasCapability(hello, 'stats');

  const inUseId = connection?.node_id ?? null;
  const nameOf = (id: string | null): string | null =>
    id == null ? null : (nodes.find((node) => node.id === id)?.name ?? id);
  const inUseName = nameOf(inUseId);
  const selectedId = settings?.selected_node ?? null;
  const selectedName = nameOf(selectedId);
  // 启动用哪个节点：连接中/已连接用数据面事实（node_id），否则用你选的那个。
  const targetNodeId = inUseId ?? selectedId;

  const status = appStatus({ loaded, stage, phase, lastError, nodeName: inUseName ?? selectedName });
  const button = startButtonView({
    stage,
    phase,
    nodeCount: nodes.length,
    nodeId: targetNodeId,
    hasTun,
    requestPending,
    lastError,
  });
  const banners = collectBanners(notices);
  const [primaryBanner, ...restBanners] = banners;
  const visibleBanners = showAllBanners ? banners : primaryBanner != null ? [primaryBanner] : [];

  // 正在用的那个节点的最近一次探测结果。`ttfb_ms` 与 `error` 恰好一个有值（契约不变量）：
  // 有 error 就是「不可用」，**不许**拿毫秒数替它背书。
  const probe = inUseId == null ? null : (nodes.find((node) => node.id === inUseId)?.probe ?? null);

  /**
   * 速率派生（设计 §4 方案 C）：
   * 契约里只有累计字节，速率必须由两个不同 `sampled_at_ms` 的样本相减得到。
   * 断开或拿不到样本时**清空基准** —— 否则下一次连接会拿跨会话的两个数字相减，得出假速率。
   */
  useEffect(() => {
    if (stage !== 'connected') {
      rateRef.current = { uplink: INITIAL_RATE, downlink: INITIAL_RATE };
      setRates({ uplink: null, downlink: null });
      return;
    }
    const up = sampleFrom(stats, 'uplink');
    const down = sampleFrom(stats, 'downlink');
    if (up == null || down == null) {
      rateRef.current = { uplink: INITIAL_RATE, downlink: INITIAL_RATE };
      setRates({ uplink: null, downlink: null });
      return;
    }
    const nextUp = deriveRate(rateRef.current.uplink, up);
    const nextDown = deriveRate(rateRef.current.downlink, down);
    rateRef.current = { uplink: nextUp, downlink: nextDown };
    setRates({ uplink: nextUp.rate, downlink: nextDown.rate });
    // 只依赖真实的采样事实：同一帧重复到达不会让基准前进。
  }, [stage, stats?.sampled_at_ms, stats?.uplink_bytes, stats?.downlink_bytes]);

  const runStart = async () => {
    if (targetNodeId == null) return;
    setRequestPending(true);
    setActionError(null);
    try {
      // 只有 TUN：产品面不再提供 proxy / 直连（用户 2026-10-01 裁决）。
      await client.connect(targetNodeId, 'tun');
    } catch (error) {
      // 失败原样展示：message 是 daemon 给的话，不是我们编的话术。
      setActionError(toDisplayError(error));
    } finally {
      setRequestPending(false);
    }
  };

  const runStop = async () => {
    setRequestPending(true);
    setActionError(null);
    try {
      await client.disconnect();
    } catch (error) {
      setActionError(toDisplayError(error));
    } finally {
      setRequestPending(false);
    }
  };

  const runProbe = async () => {
    if (nodes.length === 0) return;
    setProbing(true);
    setActionError(null);
    try {
      await client.probeNodes(nodes.map((node) => node.id));
    } catch (error) {
      setActionError(toDisplayError(error));
    } finally {
      setProbing(false);
    }
  };

  const onStartButton = () => {
    if (button.action === 'start') void runStart();
    else if (button.action === 'stop') void runStop();
    else if (button.action === 'subscribe') onNavigate('settings');
  };

  const meta: string[] = [];
  if (connection?.mode != null) meta.push(MODE_TEXT[connection.mode]);
  const connectedSince = connection?.connected_since_ms;
  if (connected && connectedSince != null) meta.push(`已运行 ${elapsedText(connectedSince, Date.now())}`);

  return (
    <div className="page dashboard dash">
      <h1 className="page__title">仪表盘</h1>

      <section className="dash__status">
        <div className={`dash__state ${DASH_TONE_CLASS[status.tone]}`} data-testid="dash-status">
          <span className={`dot ${DOT_TONE_CLASS[status.tone]}`} data-testid="dash-status-dot" aria-hidden="true" />
          <span className="dash__state-label" data-testid="dash-status-label">
            {status.label}
          </span>
          {connected && inUseName != null && (
            <>
              <span className="dash__sep">·</span>
              <span className="dash__state-node" data-testid="dash-status-node">
                {inUseName}
              </span>
              {probe?.error != null && (
                <span className="badge badge--unknown" data-testid="dash-status-probe">
                  不可用
                </span>
              )}
              {probe?.error == null && probe?.ttfb_ms != null && (
                <span className={`badge badge--${ttfbTier(probe.ttfb_ms)}`} data-testid="dash-status-probe">
                  {probe.ttfb_ms} ms
                </span>
              )}
            </>
          )}
          {/* 「你选的」与「正在用的」是两个事实。v1 没有回落（I2），所以这里只并列陈述，
              不许出现「回落 / 未使用 / 已为你切换」这类话。 */}
          {connected && selectedName != null && selectedName !== inUseName && (
            <>
              <span className="dash__sep">·</span>
              <span className="dash__state-node" data-testid="dash-selected-node">
                你选的：{selectedName}
              </span>
            </>
          )}
        </div>

        {meta.length > 0 && (
          <div className="dash__meta" data-testid="dash-meta">
            {meta.join(' · ')}
          </div>
        )}
        {status.sub != null && (
          <div className="dash__state-sub" data-testid="dash-status-sub">
            {status.sub}
          </div>
        )}

        <button
          type="button"
          className={`start${button.danger ? ' start--danger' : ''}`}
          data-testid="start-button"
          disabled={button.disabled}
          aria-busy={requestPending}
          onClick={onStartButton}
        >
          {button.label}
        </button>
        {button.note != null && (
          <div className="start__note" data-testid="start-button-note">
            {button.note}
          </div>
        )}

        <div className="row row--actions">
          <button
            type="button"
            className="btn btn--ghost"
            data-testid="dashboard-node-action"
            onClick={() => onNavigate('nodes')}
          >
            {/* 不在这里再给「添加订阅」：无节点时大启动按钮已经是那条主路径。
                这一颗只做导航（选择 / 切换节点），不会与主控件抢同一个动作。 */}
            {connected ? '切换节点' : '选择节点'}
          </button>
          <button
            type="button"
            className="btn btn--ghost"
            data-testid="dashboard-probe"
            disabled={probing || requestPending || nodes.length === 0}
            onClick={() => void runProbe()}
          >
            {probing ? '探测中…' : '测试延迟'}
          </button>
        </div>

        {actionError != null && <ErrorBox error={actionError} testId="start-action-error" />}
      </section>

      {/* 上次运行的真实失败：code 与 message 原样渲染（ErrorBox 不加工、不翻译），
          并给一个真正的下一步。这一条**不**再走横幅通道 —— 同一失败同屏两处陈述
          就是老版 task-23 D1 修掉的重复。 */}
      {lastError != null && (
        <>
          <ErrorBox error={lastError} testId="last-error" />
          <div className="row row--actions">
            <button type="button" className="btn btn--ghost" onClick={() => onNavigate('logs')}>
              查看日志
            </button>
          </div>
        </>
      )}

      {canShowStats && (
        <section className="dash__traffic" data-testid="dash-traffic">
          {stats == null ? (
            <div className="unknown" data-testid="stats-unsampled">
              未采样
            </div>
          ) : (
            <>
              <div className="dash__metric">
                <span className="dash__metric-arrow" aria-hidden="true">
                  ↓
                </span>
                <span className="dash__metric-rate" data-testid="stats-downlink-rate">
                  {rates.downlink == null ? '未采样' : formatRate(rates.downlink)}
                </span>
                <span className="dash__metric-total" data-testid="stats-downlink-total">
                  累计 {formatBytes(stats.downlink_bytes)}（{stats.downlink_bytes} 字节）
                </span>
              </div>
              <div className="dash__metric">
                <span className="dash__metric-arrow" aria-hidden="true">
                  ↑
                </span>
                <span className="dash__metric-rate" data-testid="stats-uplink-rate">
                  {rates.uplink == null ? '未采样' : formatRate(rates.uplink)}
                </span>
                <span className="dash__metric-total" data-testid="stats-uplink-total">
                  累计 {formatBytes(stats.uplink_bytes)}（{stats.uplink_bytes} 字节）
                </span>
              </div>
              <div className="dash__metric-total" data-testid="stats-sampled-at">
                最近一次采样 {formatTime(stats.sampled_at_ms)}
              </div>
            </>
          )}
        </section>
      )}

      {visibleBanners.map((banner) => (
        <div
          key={banner.key}
          className={`banner banner--${banner.tone}`}
          data-testid="dash-banner"
          role={banner.tone === 'error' ? 'alert' : 'status'}
        >
          <div className="banner__message">{banner.text}</div>
          {banner.action != null && (
            <button type="button" className="btn" onClick={() => onNavigate(banner.action!.target)}>
              {banner.action.label}
            </button>
          )}
        </div>
      ))}
      {restBanners.length > 0 && !showAllBanners && (
        <button
          type="button"
          className="dash__more"
          data-testid="dash-banner-more"
          onClick={() => setShowAllBanners(true)}
        >
          还有 {restBanners.length} 条提示
        </button>
      )}

      <details className="dash__details" data-testid="dash-diagnostics">
        <summary>诊断（数据面）</summary>
        {/* 这里只显示契约里真有的东西。老版这一块还有「Xray 核心路径 / 特权 helper / 配置路径」，
            那些字段 v1 契约里没有 —— 没有就不显示，不许拿别的值顶替。 */}
        <div className="kv">
          <div className="kv__key">数据面进程</div>
          <div className="kv__value" data-testid="datapath-pid">
            {datapath?.pid != null ? `运行中（pid ${datapath.pid}）` : '未观测到数据面进程'}
          </div>
          <div className="kv__key">核心版本</div>
          <div className="kv__value mono" data-testid="datapath-version">
            {datapath?.version ?? '未知'}
          </div>
          <div className="kv__key">核心就绪时刻</div>
          <div className="kv__value mono" data-testid="datapath-ready-at">
            {datapath?.ready_at_ms != null ? formatTime(datapath.ready_at_ms) : '未知'}
          </div>
        </div>
        <div className="row row--actions">
          <button type="button" className="btn btn--ghost" onClick={() => onNavigate('logs')}>
            查看日志
          </button>
          <button type="button" className="btn btn--ghost" onClick={() => onNavigate('settings')}>
            全部设置
          </button>
        </div>
      </details>
    </div>
  );
}
