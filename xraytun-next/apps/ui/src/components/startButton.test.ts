// startButtonView 的纯函数自检：判定表**逐行**都有断言，文案逐字比对。
//
// 为什么连 note 都逐字断言：这张表是产品与 ux 的约定，任何「顺手改个说法」
// 都应当让测试红，而不是悄悄漂移。

import { describe, expect, it } from 'vitest';
import type { StartButtonInput, StartButtonView } from './startButton';
import { startButtonView } from './startButton';

const base: StartButtonInput = {
  stage: 'disconnected',
  nodeCount: 3,
  nodeId: 'node-a',
  hasTun: true,
  requestPending: false,
  lastError: null,
};

function view(over: Partial<StartButtonInput>): StartButtonView {
  return startButtonView({ ...base, ...over });
}

describe('startButtonView 判定优先级', () => {
  it('第 1 行：stage 为 null —— 状态未知不可启动', () => {
    const v = view({ stage: null });
    expect(v.label).toBe('启动');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('正在读取状态…');
  });

  it('第 2 行：未宣告 tun_mode —— 不提供入口', () => {
    const v = view({ hasTun: false });
    expect(v.label).toBe('启动');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('当前 daemon 不提供 TUN 模式（未宣告 tun_mode 能力）');
  });

  it('第 3 行：节点目录为空 —— 引导去节点页导入', () => {
    const v = view({ nodeCount: 0 });
    expect(v.label).toBe('导入节点');
    expect(v.action).toBe('import');
    expect(v.disabled).toBe(false);
    expect(v.note).toBe('节点目录为空 —— 先在「节点」页导入订阅原文或分享链接');
  });

  it('第 4 行：请求已受理且已连接 —— 停止中', () => {
    const v = view({ stage: 'connected', requestPending: true });
    expect(v.label).toBe('停止中…');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('请求已受理，等待事件返回终态');
  });

  it('第 4 行：请求已受理且未连接 —— 启动中', () => {
    const v = view({ stage: 'disconnected', requestPending: true });
    expect(v.label).toBe('启动中…');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('请求已受理，等待事件返回终态');
  });

  it('第 5 行：连接进行中 —— 透出已知阶段', () => {
    const v = view({ stage: 'connecting', phase: 'awaiting_ready' });
    expect(v.label).toBe('启动中…');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('连接进行中：正在等待核心可连');
  });

  it('第 5 行：连接进行中但没有阶段 —— 如实写「阶段未知」', () => {
    const v = view({ stage: 'connecting' });
    expect(v.label).toBe('启动中…');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('连接进行中：阶段未知');
  });

  it('第 6 行：断开进行中', () => {
    const v = view({ stage: 'disconnecting' });
    expect(v.label).toBe('停止中…');
    expect(v.action).toBe('none');
    expect(v.disabled).toBe(true);
    expect(v.note).toBe('断开进行中');
  });

  it('第 7 行：已连接 —— 唯一的 stop', () => {
    const v = view({ stage: 'connected' });
    expect(v.label).toBe('停止');
    expect(v.action).toBe('stop');
    expect(v.disabled).toBe(false);
    expect(v.note).toBe(null);
    expect(v.danger).toBe(true);
  });

  it('第 8 行：未连接且未选择节点 —— 给的是可执行的下一步，不是死路', () => {
    const v = startButtonView({ ...base, nodeId: null });
    // 按钮**可点**（点了会导航到节点页），并且说明里写清了「点哪里」。
    expect(v.disabled).toBe(false);
    expect(v.action).toBe('select');
    expect(v.note).toContain('选为当前节点');
    expect(v.note).toContain('选择节点');
    expect(v.danger).toBe(false);
  });

  it('第 9 行：未连接、有节点、无上次错误 —— 可以启动', () => {
    const v = view({ stage: 'disconnected', nodeId: 'node-b' });
    expect(v.label).toBe('启动');
    expect(v.action).toBe('start');
    expect(v.disabled).toBe(false);
    expect(v.note).toBe(null);
  });

  it('第 9 行：未连接、有节点、有上次错误 —— 错误逐字附在按钮下方', () => {
    const v = view({
      stage: 'disconnected',
      nodeId: 'node-b',
      lastError: { code: 'core_exited_early', message: '核心进程提前退出' },
    });
    expect(v.label).toBe('启动');
    expect(v.action).toBe('start');
    expect(v.disabled).toBe(false);
    expect(v.note).toBe('上次运行出错：核心进程提前退出');
  });

  it('danger 仅在 action === stop 时为 true', () => {
    const cases: StartButtonInput[] = [
      { ...base, stage: null },
      { ...base, hasTun: false },
      { ...base, nodeCount: 0 },
      { ...base, stage: 'disconnected', requestPending: true },
      { ...base, stage: 'connecting', phase: 'starting_core' },
      { ...base, stage: 'disconnecting' },
      { ...base, stage: 'connected' },
      { ...base, nodeId: null },
      { ...base, nodeId: 'node-b' },
    ];
    const views = cases.map(startButtonView);
    for (const v of views) {
      if (v.action === 'stop') {
        expect(v.danger).toBe(true);
      } else {
        expect(v.danger).toBe(false);
      }
    }
    // 至少有一个 stop 与一个非 stop，否则上面的循环是空转。
    expect(views.some((v) => v.action === 'stop')).toBe(true);
    expect(views.some((v) => v.action !== 'stop')).toBe(true);
  });
});
