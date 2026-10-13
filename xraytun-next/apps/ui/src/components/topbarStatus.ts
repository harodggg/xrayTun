// 顶栏与仪表盘共用的状态派生：一个纯函数决定 tone / label / sub / detail。
//
// 为什么把三套 class 映射也放在这里：tone 是语义（on / partial / off / busy / failed），
// class 只是它在某个位置的具体拼写。分散到各组件里就会出现「同一个 tone 在不同页面
// 映射成不同颜色」这种没法验收的分歧。
//
// 两条容易写错的语义边界：
//  - `!loaded`（还没拿到 daemon 状态）不等于「未连接」，所以文案与 detail 都显式区分；
//  - `committing_routes` 时隧道已建立但默认路由尚未接管 —— 这是 partial，不是 on，
//    也不是 connecting 的普通中间态。

import type { ConnectPhase, ErrorBody, Stage } from '../transport/contract';
import { PHASE_TEXT } from './format';

export type StatusTone = 'on' | 'partial' | 'off' | 'busy' | 'failed';

export interface StatusInput {
  loaded: boolean; // connection !== null
  stage: Stage | null;
  phase?: ConnectPhase;
  lastError: ErrorBody | null;
  nodeName: string | null;
}

export interface AppStatus {
  tone: StatusTone;
  label: string;
  sub: string | null;
  detail: string;
}

export const TOPBAR_TONE_CLASS: Record<StatusTone, string> = {
  on: 'topbar--on',
  partial: 'topbar--partial',
  off: 'topbar--off',
  busy: 'topbar--busy',
  failed: 'topbar--failed',
};

export const DOT_TONE_CLASS: Record<StatusTone, string> = {
  on: 'dot--on',
  partial: 'dot--partial',
  off: 'dot--off',
  busy: 'dot--warn',
  failed: 'dot--failed',
};

export const DASH_TONE_CLASS: Record<StatusTone, string> = {
  on: 'dash__state--on',
  partial: 'dash__state--partial',
  off: 'dash__state--off',
  busy: 'dash__state--busy',
  failed: 'dash__state--failed',
};

const NOT_CONNECTED_DETAIL = '未连接 —— 没有流量走代理';

export function appStatus(input: StatusInput): AppStatus {
  const { loaded, stage, phase, lastError, nodeName } = input;

  // 1. 还没拿到状态：不等于「未连接」，要说得更重一些。
  if (!loaded) {
    return {
      tone: 'off',
      label: '正在读取状态…',
      sub: null,
      detail: '还没有拿到 daemon 状态 —— 现在无法判断流量是否受保护（这不等于「未连接」）',
    };
  }

  // 2. 上次运行失败，且当前确实处于未连接。
  if (lastError != null && stage === 'disconnected') {
    return {
      tone: 'failed',
      label: '上次运行出错',
      sub: lastError.message,
      detail: `上次运行出错：${lastError.message}`,
    };
  }

  // 3. 明确的未连接（无错误）。
  if (stage === 'disconnected') {
    return {
      tone: 'off',
      label: '未连接',
      sub: null,
      detail: NOT_CONNECTED_DETAIL,
    };
  }

  // 4. 隧道已建立，但默认路由还没接管：部分可用，不能显示成已连接。
  if (stage === 'connecting' && phase === 'committing_routes') {
    const label = '隧道已建立，默认路由尚未接管';
    return {
      tone: 'partial',
      label,
      sub: null,
      detail: label,
    };
  }

  // 5. 连接中：子阶段未知就写「阶段未知」。
  if (stage === 'connecting') {
    const phaseText = phase != null ? PHASE_TEXT[phase] : undefined;
    const sub = phaseText ?? '阶段未知';
    return {
      tone: 'busy',
      label: '连接中',
      sub,
      detail: `连接中 —— ${sub}`,
    };
  }

  // 6. 已连接：节点名未知就写「节点未知」。
  if (stage === 'connected') {
    return {
      tone: 'on',
      label: '已连接',
      sub: null,
      detail: `已连接 · ${nodeName ?? '节点未知'}`,
    };
  }

  // 7. 断开中。
  if (stage === 'disconnecting') {
    return {
      tone: 'busy',
      label: '断开中',
      sub: null,
      detail: '断开中',
    };
  }

  // 8. 已加载但 stage 缺失：按未连接呈现。
  return {
    tone: 'off',
    label: '未连接',
    sub: null,
    detail: NOT_CONNECTED_DETAIL,
  };
}
