import { useEffect, useState } from 'react';
import { ErrorBox } from '../components/ErrorBox';
import {
  LOG_LEVELS,
  formatTime,
  hasCapability,
  toDisplayError,
  toDisplayErrorBody,
} from '../components/format';
import type { DisplayError } from '../components/format';
import {
  canInstallHelper,
  helperStatus,
  installHelper,
  restartHelper,
  uninstallHelper,
} from '../shell';
import type { HelperStatus } from '../shell';
import { useDaemon } from '../store/daemon';
import type { LogLevel } from '../transport/contract';

/** helper 状态行：四件客观事实之一定档，不做任何推断。 */
function helperStateText(status: HelperStatus): string {
  if (status.reachable) return '已安装 · 正在服务';
  if (status.installed) return '已安装，但连不上（socket 没有在服务）';
  return '未安装 —— TUN 模式无法使用';
}

/** helper 细节行：把三个文件级事实逐项摊开，方便用户/排障看清缺的是哪一件。 */
function helperDetailText(status: HelperStatus): string {
  const binary = status.installed ? '已装' : '未装';
  const plist = status.plist_present ? '在' : '不在';
  const socket = status.socket_present ? '在' : '不在';
  return `二进制：${binary} · launchd 配置：${plist} · socket：${socket}`;
}

type HelperAction = 'install' | 'uninstall' | 'restart';

/**
 * 特权助手卡片：只在壳注入了 `__XT_HELPER_INSTALL__` 时渲染（由调用方门控）。
 *
 * TUN 模式要建 VPN 网卡、改路由与 DNS，这两件事必须由 root 做 —— 没有装 helper 时
 * 界面必须给出**真正的下一步**，而不是只报 `helper_unavailable`（死路 D4）。
 * 三个动作都直接打到壳命令上；成功后重新拉一次 status，显示以壳的事实为准。
 */
function HelperCard() {
  const [status, setStatus] = useState<HelperStatus | null>(null);
  const [pending, setPending] = useState<HelperAction | null>(null);
  const [error, setError] = useState<DisplayError | null>(null);
  const [confirming, setConfirming] = useState(false);

  useEffect(() => {
    let active = true;
    void helperStatus().then(
      (next) => {
        if (active) setStatus(next);
      },
      (cause: unknown) => {
        if (active) setError(toDisplayError(cause));
      },
    );
    return () => {
      active = false;
    };
  }, []);

  const run = async (key: HelperAction, action: () => Promise<void>) => {
    setPending(key);
    setError(null);
    try {
      await action();
      // 动作成功不等于状态就是自己想象的那样：以壳重新给出的客观事实为准。
      setStatus(await helperStatus());
    } catch (cause) {
      // 壳 reject 的是字符串，原样交给 ErrorBox，不改写、不翻译。
      setError(toDisplayError(cause));
    } finally {
      setPending(null);
      setConfirming(false);
    }
  };

  return (
    <section className="card helper-card" data-testid="helper-card">
      <div className="card__title">特权助手（TUN 需要）</div>
      <div className="card__body">
        <p className="card__desc">
          TUN 模式需要它来创建 VPN 网卡、改路由与 DNS —— 这两件事必须由 root 做，所以 App
          会请求一次管理员授权，把它装成一个后台服务。它只认识固定指令集（连接/断开/回滚），不接受任意命令；卸载会把它和它的
          launchd 配置一起删掉。
        </p>
        {status != null && (
          <div className="helper__status">
            <div className="helper__state" data-testid="helper-status">
              {helperStateText(status)}
            </div>
            <div className="helper__detail" data-testid="helper-detail">
              {helperDetailText(status)}
            </div>
          </div>
        )}
        <div className="helper__actions">
          {status?.installed === false && (
            <button
              type="button"
              className="btn btn--primary"
              data-testid="helper-install-button"
              disabled={pending != null}
              onClick={() => void run('install', installHelper)}
            >
              {pending === 'install' ? '安装中…' : '安装特权助手'}
            </button>
          )}
          {status?.installed === true && status.reachable === false && (
            <button
              type="button"
              className="btn btn--primary"
              data-testid="helper-restart-button"
              disabled={pending != null}
              onClick={() => void run('restart', restartHelper)}
            >
              {pending === 'restart' ? '重启中…' : '重启助手'}
            </button>
          )}
          {status?.installed === true && (
            <button
              type="button"
              className={`btn${confirming ? ' btn--danger helper__confirm' : ''}`}
              data-testid={confirming ? 'helper-uninstall-confirm' : 'helper-uninstall-button'}
              disabled={pending != null}
              onClick={() => {
                // 第一次只是把这一颗变成确认态：卸载会把服务与它的 launchd 配置一起删掉，
                // 不该因为一次误触就发生。
                if (!confirming) {
                  setConfirming(true);
                  return;
                }
                void run('uninstall', uninstallHelper);
              }}
            >
              {pending === 'uninstall' ? '卸载中…' : confirming ? '确认卸载？' : '卸载助手'}
            </button>
          )}
        </div>
        {error != null && <ErrorBox error={error} testId="helper-error" />}
      </div>
    </section>
  );
}

// 设置页的每个控件都对应契约里真实存在的字段：
// socks_listen / log_level → SettingsPatch（本轮真实现，保存后以 daemon 的值为准）。
// 订阅按能力粒度分两层：'subscriptions' 才显示只读列表，
// 'subscription_fetch' 才显示「添加 / 刷新」入口 —— 没宣告就不渲染，也不写占位文案。
//
// 「草稿」为 null 表示跟随 daemon 的真实值：本地状态不能长期压过服务端事实。
export function Settings() {
  const { settings, hello, subscriptions, client } = useDaemon();
  const [socksDraft, setSocksDraft] = useState<string | null>(null);
  const [levelDraft, setLevelDraft] = useState<LogLevel | null>(null);
  const [urlDraft, setUrlDraft] = useState('');
  const [pending, setPending] = useState<string | null>(null);
  const [actionError, setActionError] = useState<DisplayError | null>(null);
  // 错误提示渲染在**出错的那个区块里**：设置页很长，订阅卡片在最下面，
  // 把「添加订阅失败」写到页面顶部等于让用户看不到（真人走查发现的缺口）。
  const [errorScope, setErrorScope] = useState<'page' | 'subscriptions'>('page');

  const socksValue = socksDraft ?? settings?.socks_listen ?? '';
  const levelValue = levelDraft ?? settings?.log_level ?? null;
  const settingsLoaded = settings != null;
  const socksChanged = settingsLoaded && socksDraft != null && socksDraft !== settings?.socks_listen;
  const levelChanged = settingsLoaded && levelDraft != null && levelDraft !== settings?.log_level;
  const canReadSubscriptions = hasCapability(hello, 'subscriptions');
  const canFetchSubscriptions = hasCapability(hello, 'subscription_fetch');

  const run = async (
    key: string,
    action: () => Promise<unknown>,
    onSuccess?: () => void,
    scope: 'page' | 'subscriptions' = 'page',
  ) => {
    setPending(key);
    setActionError(null);
    setErrorScope(scope);
    try {
      await action();
      if (onSuccess != null) onSuccess();
    } catch (error) {
      setActionError(toDisplayError(error));
    } finally {
      setPending(null);
    }
  };

  return (
    <div className="page settings" data-testid="page-settings">
      <h1 className="page__title">设置</h1>

      {actionError != null && errorScope === 'page' && (
        <ErrorBox error={actionError} testId="settings-error" />
      )}

      {/* 第一张卡片：TUN 死路 D4 的修复点。壳没注入标记时整块不出现 ——
          装不了的壳不该摆一个按不动的假控件。 */}
      {canInstallHelper() && <HelperCard />}

      <section className="card">
        <div className="card__title">daemon</div>
        <div className="card__body">
          <div className="row">
            <span className="row__label">版本</span>
            <span className="row__value mono" data-testid="daemon-version">
              {hello != null ? hello.daemon_version : '未知'}
            </span>
          </div>
          <div className="row">
            <span className="row__label">pid</span>
            <span className="row__value mono" data-testid="daemon-pid">
              {hello != null ? hello.pid : '未知'}
            </span>
          </div>
          {hello != null && (
            <>
              <div className="row">
                <span className="row__label">协议版本</span>
                <span className="row__value mono" data-testid="daemon-protocol-version">
                  {hello.protocol_version}
                </span>
              </div>
              <div className="row">
                <span className="row__label">启动时刻</span>
                <span className="row__value mono" data-testid="daemon-started-at">
                  {formatTime(hello.started_at_ms)}
                </span>
              </div>
              <div className="row">
                <span className="row__label">能力</span>
                <span className="row__value" data-testid="daemon-capabilities">
                  {hello.capabilities.length === 0
                    ? '未宣告任何能力'
                    : hello.capabilities.join('、')}
                </span>
              </div>
            </>
          )}
        </div>
      </section>

      <section className="card">
        <div className="card__title">代理入口</div>
        <div className="card__body">
          <div className="field">
            <label className="field__label" htmlFor="socks-listen">
              SOCKS 监听地址
            </label>
            <input
              id="socks-listen"
              className="input mono"
              data-testid="socks-listen-input"
              type="text"
              value={socksValue}
              disabled={!settingsLoaded || pending === 'socks'}
              onChange={(event) => setSocksDraft(event.target.value)}
            />
            {!settingsLoaded && <span className="field__hint muted">设置未加载，无法编辑。</span>}
          </div>
          <button
            className="btn btn--primary"
            data-testid="save-socks-button"
            disabled={!socksChanged || pending != null}
            onClick={() =>
              void run(
                'socks',
                () => client.patchSettings({ socks_listen: socksValue }),
                () => setSocksDraft(null),
              )
            }
          >
            保存监听地址
          </button>
        </div>
      </section>

      <section className="card">
        <div className="card__title">日志级别</div>
        <div className="card__body">
          <select
            className="select"
            data-testid="log-level-select"
            value={levelValue ?? ''}
            disabled={!settingsLoaded || pending === 'level'}
            aria-label="日志级别"
            onChange={(event) => setLevelDraft(event.target.value as LogLevel)}
          >
            {levelValue == null && <option value="">未知</option>}
            {LOG_LEVELS.map((level) => (
              <option value={level} key={level}>
                {level}
              </option>
            ))}
          </select>
          <button
            className="btn btn--primary"
            data-testid="save-log-level-button"
            disabled={!levelChanged || pending != null}
            onClick={() =>
              void run(
                'level',
                () => client.patchSettings({ log_level: levelDraft as LogLevel }),
                () => setLevelDraft(null),
              )
            }
          >
            保存日志级别
          </button>
        </div>
      </section>

      {canReadSubscriptions && (
        <section className="card" data-testid="subscriptions-section">
          <div className="card__title">订阅</div>
          <div className="card__body">
            {canReadSubscriptions && !canFetchSubscriptions && (
              <p className="page__hint" data-testid="subscription-import-hint">
                远端拉取本版本不提供。要到「节点」页粘贴订阅原文或分享链接导入。
              </p>
            )}
            {actionError != null && errorScope === 'subscriptions' && (
              <ErrorBox error={actionError} testId="settings-error" />
            )}
            {canFetchSubscriptions && (
              <form
                className="row"
                onSubmit={(event) => {
                  event.preventDefault();
                  const url = urlDraft.trim();
                  if (url === '') return;
                  void run(
                    'add-sub',
                    () => client.addSubscription(url),
                    () => setUrlDraft(''),
                    'subscriptions',
                  );
                }}
              >
                <input
                  className="input mono"
                  data-testid="subscription-url-input"
                  type="text"
                  aria-label="订阅地址"
                  value={urlDraft}
                  disabled={pending === 'add-sub'}
                  onChange={(event) => setUrlDraft(event.target.value)}
                />
                <button
                  className="btn btn--primary"
                  type="submit"
                  data-testid="add-subscription-button"
                  disabled={urlDraft.trim() === '' || pending != null}
                >
                  添加订阅
                </button>
              </form>
            )}

            {subscriptions.length === 0 ? (
              <div className="page__empty" data-testid="subscriptions-empty">
                <p className="page__empty-title">暂无订阅</p>
                <p className="page__empty-hint">订阅列表来自 daemon 的真实记录。</p>
              </div>
            ) : (
              <div className="subs__list">
                {subscriptions.map((subscription) => {
                  const subError = toDisplayErrorBody(subscription.last_error);
                  return (
                    <div className="subs__row" data-testid="subscription-row" key={subscription.id}>
                      <div className="subs__url mono">{subscription.url}</div>
                      <div className="subs__meta">
                        <span className="mono" data-testid="subscription-node-count">
                          {subscription.node_count} 个节点
                        </span>
                        <span className="muted mono" data-testid="subscription-fetched-at">
                          {subscription.fetched_at_ms != null
                            ? `拉取于 ${formatTime(subscription.fetched_at_ms)}`
                            : '尚未拉取'}
                        </span>
                      </div>
                      {subError != null && (
                        <ErrorBox error={subError} testId="subscription-last-error" />
                      )}
                      {canFetchSubscriptions && (
                        <button
                          className="btn"
                          data-testid="refresh-subscription-button"
                          disabled={pending === `refresh-${subscription.id}`}
                          onClick={() =>
                            void run(
                              `refresh-${subscription.id}`,
                              () => client.refreshSubscription(subscription.id),
                              undefined,
                              'subscriptions',
                            )
                          }
                        >
                          刷新
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </section>
      )}
    </div>
  );
}
