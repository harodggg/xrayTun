import { useState } from 'react';
import { ErrorBox } from '../components/ErrorBox';
import {
  formatTime,
  hasCapability,
  toDisplayError,
  toDisplayErrorBody,
} from '../components/format';
import type { DisplayError } from '../components/format';
import { useDaemon } from '../store/daemon';

// 节点页只画节点目录里真实存在的条目，不做任何「示例节点」。
// 探测入口由 DaemonHello.capabilities 门控；已有探测结果不需要入口也能显示。
export function Nodes() {
  const { connection, nodes, settings, hello, client } = useDaemon();
  const [probePending, setProbePending] = useState<Record<string, boolean>>({});
  const [actionPending, setActionPending] = useState<string | null>(null);
  const [actionError, setActionError] = useState<DisplayError | null>(null);

  const stage = connection?.stage ?? null;
  const connected = stage === 'connected';
  const currentId = connection?.node_id ?? null;
  const selectedId = settings?.selected_node ?? null;
  const canProbe = hasCapability(hello, 'probe');

  const onProbe = async (nodeId: string) => {
    setProbePending((pending) => ({ ...pending, [nodeId]: true }));
    setActionError(null);
    try {
      await client.probeNodes([nodeId]);
    } catch (error) {
      setActionError(toDisplayError(error));
    } finally {
      // 这里只负责「请求往返」的进行中状态；TTFB 结果通过事件进入 store 后由渲染读取。
      setProbePending((pending) => {
        const next = { ...pending };
        delete next[nodeId];
        return next;
      });
    }
  };

  const runNodeAction = async (nodeId: string, action: () => Promise<unknown>) => {
    setActionPending(nodeId);
    setActionError(null);
    try {
      await action();
    } catch (error) {
      setActionError(toDisplayError(error));
    } finally {
      setActionPending(null);
    }
  };

  return (
    <div className="page nodes" data-testid="page-nodes">
      <h1 className="page__title">节点</h1>
      <p className="page__hint" data-testid="switch-policy">
        选择就用：失败会如实报错，不会自动换下一个节点。
      </p>

      {actionError != null && <ErrorBox error={actionError} testId="nodes-action-error" />}

      {nodes.length === 0 ? (
        <div className="page__empty" data-testid="nodes-empty">
          <p className="page__empty-title">暂无节点</p>
          <p className="page__empty-hint">
            节点目录为空。本页不会填充示例节点，也不会给出无法兑现的操作。
          </p>
        </div>
      ) : (
        <div className="nodes__list">
          {nodes.map((node) => {
            const isCurrent = connected && currentId === node.id;
            const isSelected = selectedId === node.id;
            const probeError = toDisplayErrorBody(node.probe?.error);
            const showProbe = canProbe || node.probe != null;
            const rowClass = [
              'nodes__row',
              isCurrent ? 'nodes__row--current' : '',
              isSelected ? 'nodes__row--selected' : '',
            ]
              .filter(Boolean)
              .join(' ');
            return (
              <div className={rowClass} data-testid="node-row" key={node.id}>
                <div className="nodes__name">
                  <span data-testid="node-name">{node.name}</span>
                  {isCurrent && (
                    <span className="badge badge--connected" data-testid="node-current-badge">
                      当前
                    </span>
                  )}
                </div>
                <div className="nodes__meta">
                  <span className="nodes__protocol mono" data-testid="node-protocol">
                    {node.protocol}
                  </span>
                  <span className="nodes__endpoint mono" data-testid="node-endpoint">
                    {node.endpoint}
                  </span>
                  <span
                    className={`nodes__source nodes__source--${node.source.kind}`}
                    data-testid="node-source"
                  >
                    {node.source.kind === 'subscription'
                      ? `订阅 ${node.source.id}`
                      : '手动添加'}
                  </span>
                </div>
                {showProbe && (
                  <div className="nodes__probe" data-testid="node-probe">
                    {node.probe == null ? (
                      <span className="probe probe--none unknown">未探测</span>
                    ) : node.probe.ttfb_ms != null ? (
                      <span className="probe probe--ok mono">
                        TTFB {node.probe.ttfb_ms} ms
                        <span className="muted"> · {formatTime(node.probe.at_ms)}</span>
                      </span>
                    ) : probeError != null ? (
                      <div className="probe probe--fail">
                        <ErrorBox error={probeError} testId="node-probe-error" />
                      </div>
                    ) : (
                      // ProbeResult 的不变量是「恰好一个字段有值」；两个都没有时
                      // 说明线上数据不自洽，如实显示未知而不是猜一个。
                      <span className="unknown">未知</span>
                    )}
                  </div>
                )}
                <div className="nodes__actions">
                  {canProbe && (
                    <button
                      className="btn"
                      data-testid="probe-button"
                      disabled={probePending[node.id] === true}
                      onClick={() => void onProbe(node.id)}
                    >
                      {probePending[node.id] === true ? '探测中…' : '探测'}
                    </button>
                  )}
                  {connected ? (
                    <button
                      className="btn btn--primary"
                      data-testid="switch-button"
                      disabled={isCurrent || actionPending === node.id}
                      onClick={() =>
                        void runNodeAction(node.id, () => client.switchNode(node.id))
                      }
                    >
                      {isCurrent ? '当前节点' : '切换并立即使用'}
                    </button>
                  ) : (
                    <button
                      className="btn"
                      data-testid="select-button"
                      disabled={isSelected || actionPending === node.id}
                      onClick={() =>
                        void runNodeAction(node.id, () =>
                          client.patchSettings({ selected_node: node.id }),
                        )
                      }
                    >
                      {isSelected ? '已选择' : '选为当前节点'}
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
