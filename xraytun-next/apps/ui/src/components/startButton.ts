// 启动/停止按钮的纯派生逻辑：把「当前能做什么」写成一个无副作用函数，
// 让渲染层只负责把结果画出来。
//
// 这里的优先级顺序就是产品语义，先命中先返回：
// 「状态未知」与「daemon 没有 TUN 能力」都不可启动（不是暂时不可用，是本版本不提供入口）；
// 「节点目录为空」优先引导去添加订阅，而不是给一个注定失败的启动按钮；
// 「请求已受理」期间一律禁用，等事件流把终态推回来 —— 不排队、不猜测、不自动重放。
//
// 未知就是未知：没有阶段资料时文案写「阶段未知」，不编一个默认阶段。

import type { ConnectPhase, ErrorBody, Stage } from '../transport/contract';
import { PHASE_TEXT } from './format';

export interface StartButtonInput {
  stage: Stage | null; // null = 还没拿到状态
  phase?: ConnectPhase;
  nodeCount: number;
  nodeId: string | null;
  hasTun: boolean; // hello.capabilities 含 'tun_mode'
  requestPending: boolean;
  lastError: ErrorBody | null;
}

export interface StartButtonView {
  label: string;
  action: 'start' | 'stop' | 'subscribe' | 'none';
  disabled: boolean;
  danger: boolean; // true 仅当 action === 'stop'
  note: string | null; // 按钮下方说明（逐字）
}

export function startButtonView(input: StartButtonInput): StartButtonView {
  const { stage, phase, nodeCount, nodeId, hasTun, requestPending, lastError } = input;

  // 1. 连状态快照都没有：不给入口，也不假设「未连接」。
  if (stage === null) {
    return {
      label: '启动',
      action: 'none',
      disabled: true,
      danger: false,
      note: '正在读取状态…',
    };
  }

  // 2. 能力事实源：daemon 没宣告 tun_mode，本版本就没有这条路径。
  if (!hasTun) {
    return {
      label: '启动',
      action: 'none',
      disabled: true,
      danger: false,
      note: '当前 daemon 不提供 TUN 模式（未宣告 tun_mode 能力）',
    };
  }

  // 3. 没有节点可选：启动之前先得有一个目录。
  if (nodeCount === 0) {
    return {
      label: '添加订阅',
      action: 'subscribe',
      disabled: false,
      danger: false,
      note: '节点目录为空 —— 启动前先添加订阅',
    };
  }

  // 4. 请求已受理：终态一定在事件流里出现，这里只负责不重复发请求。
  if (requestPending) {
    return {
      label: stage === 'connected' ? '停止中…' : '启动中…',
      action: 'none',
      disabled: true,
      danger: false,
      note: '请求已受理，等待事件返回终态',
    };
  }

  // 5. 连接进行中：把子阶段如实透出。
  if (stage === 'connecting') {
    const phaseText = phase != null ? PHASE_TEXT[phase] : undefined;
    return {
      label: '启动中…',
      action: 'none',
      disabled: true,
      danger: false,
      note: `连接进行中：${phaseText ?? '阶段未知'}`,
    };
  }

  // 6. 断开进行中。
  if (stage === 'disconnecting') {
    return {
      label: '停止中…',
      action: 'none',
      disabled: true,
      danger: false,
      note: '断开进行中',
    };
  }

  // 7. 已连接：唯一允许 danger 的终态。
  if (stage === 'connected') {
    return {
      label: '停止',
      action: 'stop',
      disabled: false,
      danger: true,
      note: null,
    };
  }

  // 8. 未连接且没有选中节点。
  if (nodeId === null) {
    return {
      label: '启动',
      action: 'none',
      disabled: true,
      danger: false,
      note: '未选择节点',
    };
  }

  // 9. 未连接、有节点：可以启动；上次失败如实挂在按钮下方。
  return {
    label: '启动',
    action: 'start',
    disabled: false,
    danger: false,
    note: lastError != null ? `上次运行出错：${lastError.message}` : null,
  };
}
