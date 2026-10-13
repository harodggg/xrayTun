// 顶栏（老版「开始面板」的全局状态条）：只报告状态，不再放第二颗启动按钮。
//
// 为什么没有按钮：老版 task-72 把「连接/断开」收敛到全屏唯一一处；本版那一处是
// 仪表板顶部的大启动按钮（见 pages/Dashboard.tsx）。同屏出现两个「启动/停止」只是重复，
// 而且两处的禁用判据早晚会漂移。
//
// 状态词 / 圆点 / 颜色的唯一真源是 components/topbarStatus.ts —— 顶栏与仪表板必须同源，
// 同一事实不许写两份判据。

import { useDaemon } from '../store/daemon';
import { DOT_TONE_CLASS, TOPBAR_TONE_CLASS, appStatus } from './topbarStatus';

export function Topbar() {
  const { connection, nodes, settings } = useDaemon();

  const loaded = connection !== null;
  const stage = connection?.stage ?? null;
  const inUseId = connection?.node_id ?? null;
  // 显示哪个节点：正在用的优先（连接后 node_id 就是数据面事实），否则显示你选的那个。
  const shownId = inUseId ?? settings?.selected_node ?? null;
  const nodeName = shownId == null ? null : (nodes.find((node) => node.id === shownId)?.name ?? shownId);

  const status = appStatus({
    loaded,
    stage,
    phase: connection?.phase,
    lastError: connection?.last_error ?? null,
    nodeName,
  });

  return (
    <header className={`topbar ${TOPBAR_TONE_CLASS[status.tone]}`} data-testid="topbar">
      <span className={`dot ${DOT_TONE_CLASS[status.tone]}`} data-testid="topbar-dot" aria-hidden="true" />
      {/* 状态词进 live region：状态变化时读屏要能播报，不能只靠颜色。 */}
      <span className="topbar__badge" data-testid="topbar-status" role="status" title={status.detail}>
        {status.label}
      </span>
      {status.sub != null && (
        <span className="topbar__badge topbar__badge--sub" data-testid="topbar-sub">
          {status.sub}
        </span>
      )}
      {nodeName != null && (
        <span className="topbar__node" data-testid="topbar-node">
          {nodeName}
        </span>
      )}
    </header>
  );
}
