// appStatus 的纯函数自检：判定表逐行断言 tone / label / sub，三个 class 映射
// 断言键齐全且值逐字正确（它们是 CSS 的契约，错一个字母就是「没有样式」）。
//
// detail 只在关键分行上断言（它是面向用户的完整句子，与 label/sub 同源）。

import { describe, expect, it } from 'vitest';
import type { StatusInput } from './topbarStatus';
import { appStatus, DASH_TONE_CLASS, DOT_TONE_CLASS, TOPBAR_TONE_CLASS } from './topbarStatus';

const base: StatusInput = {
  loaded: true,
  stage: 'connected',
  nodeName: '东京 · 01',
  lastError: null,
};

function status(over: Partial<StatusInput>) {
  return appStatus({ ...base, ...over });
}

describe('appStatus 判定优先级', () => {
  it('第 1 行：还没加载 —— 未知不等于未连接', () => {
    const s = status({ loaded: false, stage: null });
    expect(s.tone).toBe('off');
    expect(s.label).toBe('正在读取状态…');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe(
      '还没有拿到 daemon 状态 —— 现在无法判断流量是否受保护（这不等于「未连接」）',
    );
  });

  it('第 2 行：未连接且有上次错误', () => {
    const s = status({
      stage: 'disconnected',
      lastError: { code: 'core_exited_early', message: '核心进程提前退出' },
    });
    expect(s.tone).toBe('failed');
    expect(s.label).toBe('上次运行出错');
    expect(s.sub).toBe('核心进程提前退出');
    expect(s.detail).toBe('上次运行出错：核心进程提前退出');
  });

  it('第 3 行：未连接（无错误）', () => {
    const s = status({ stage: 'disconnected' });
    expect(s.tone).toBe('off');
    expect(s.label).toBe('未连接');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('未连接 —— 没有流量走代理');
  });

  it('第 4 行：connecting 且正在提交路由 —— partial 而非 on', () => {
    const s = status({ stage: 'connecting', phase: 'committing_routes' });
    expect(s.tone).toBe('partial');
    expect(s.label).toBe('隧道已建立，默认路由尚未接管');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('隧道已建立，默认路由尚未接管');
  });

  it('第 5 行：connecting —— sub 透出子阶段', () => {
    const s = status({ stage: 'connecting', phase: 'starting_core' });
    expect(s.tone).toBe('busy');
    expect(s.label).toBe('连接中');
    expect(s.sub).toBe('正在启动核心进程');
    expect(s.detail).toBe('连接中 —— 正在启动核心进程');
  });

  it('第 5 行：connecting 但阶段未知', () => {
    const s = status({ stage: 'connecting' });
    expect(s.tone).toBe('busy');
    expect(s.label).toBe('连接中');
    expect(s.sub).toBe('阶段未知');
    expect(s.detail).toBe('连接中 —— 阶段未知');
  });

  it('第 6 行：已连接 —— detail 带节点名', () => {
    const s = status({ stage: 'connected', nodeName: '东京 · 01' });
    expect(s.tone).toBe('on');
    expect(s.label).toBe('已连接');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('已连接 · 东京 · 01');
  });

  it('第 6 行：已连接但没有节点名 —— 不编造名字', () => {
    const s = status({ stage: 'connected', nodeName: null });
    expect(s.tone).toBe('on');
    expect(s.label).toBe('已连接');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('已连接 · 节点未知');
  });

  it('第 7 行：断开中', () => {
    const s = status({ stage: 'disconnecting' });
    expect(s.tone).toBe('busy');
    expect(s.label).toBe('断开中');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('断开中');
  });

  it('第 8 行：已加载但 stage 缺失 —— 按未连接呈现', () => {
    const s = status({ stage: null });
    expect(s.tone).toBe('off');
    expect(s.label).toBe('未连接');
    expect(s.sub).toBe(null);
    expect(s.detail).toBe('未连接 —— 没有流量走代理');
  });
});

describe('tone → class 映射', () => {
  const TONES = ['on', 'partial', 'off', 'busy', 'failed'] as const;

  it('三个映射的键齐全', () => {
    expect(Object.keys(TOPBAR_TONE_CLASS).sort()).toEqual([...TONES].sort());
    expect(Object.keys(DOT_TONE_CLASS).sort()).toEqual([...TONES].sort());
    expect(Object.keys(DASH_TONE_CLASS).sort()).toEqual([...TONES].sort());
  });

  it('TOPBAR_TONE_CLASS 值逐字正确', () => {
    expect(TOPBAR_TONE_CLASS).toEqual({
      on: 'topbar--on',
      partial: 'topbar--partial',
      off: 'topbar--off',
      busy: 'topbar--busy',
      failed: 'topbar--failed',
    });
  });

  it('DOT_TONE_CLASS 值逐字正确', () => {
    expect(DOT_TONE_CLASS).toEqual({
      on: 'dot--on',
      partial: 'dot--partial',
      off: 'dot--off',
      busy: 'dot--warn',
      failed: 'dot--failed',
    });
  });

  it('DASH_TONE_CLASS 值逐字正确', () => {
    expect(DASH_TONE_CLASS).toEqual({
      on: 'dash__state--on',
      partial: 'dash__state--partial',
      off: 'dash__state--off',
      busy: 'dash__state--busy',
      failed: 'dash__state--failed',
    });
  });
});
