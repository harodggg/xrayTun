import { useState } from 'react';
import { ErrorBox } from '../components/ErrorBox';
import {
  LOG_LEVELS,
  formatTime,
  hasCapability,
  toDisplayError,
  toDisplayErrorBody,
} from '../components/format';
import type { DisplayError } from '../components/format';
import { useDaemon } from '../store/daemon';
import type { LogLevel } from '../transport/contract';

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

  const socksValue = socksDraft ?? settings?.socks_listen ?? '';
  const levelValue = levelDraft ?? settings?.log_level ?? null;
  const settingsLoaded = settings != null;
  const socksChanged = settingsLoaded && socksDraft != null && socksDraft !== settings?.socks_listen;
  const levelChanged = settingsLoaded && levelDraft != null && levelDraft !== settings?.log_level;
  const canReadSubscriptions = hasCapability(hello, 'subscriptions');
  const canFetchSubscriptions = hasCapability(hello, 'subscription_fetch');

  const run = async (key: string, action: () => Promise<unknown>, onSuccess?: () => void) => {
    setPending(key);
    setActionError(null);
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

      {actionError != null && <ErrorBox error={actionError} testId="settings-error" />}

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
                            void run(`refresh-${subscription.id}`, () =>
                              client.refreshSubscription(subscription.id),
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
