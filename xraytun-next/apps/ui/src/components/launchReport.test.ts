// 纯函数单测：壳的启动结论 → 一句人话。每条分支都在这里钉住措辞，
// 因为「没能确认在监听」与「启动失败」的把握不同，界面不许把两者糊成一句。

import { describe, expect, it } from 'vitest';

import { launchLogLine, launchReportText } from './launchReport';
import type { LaunchReport } from '../shell';

function report(overrides: Partial<LaunchReport> = {}): LaunchReport {
  return {
    outcome: 'unavailable',
    reason: '没找到 xt-daemon：设 XT_DAEMON_BIN 指向它',
    pid: null,
    bin: null,
    socket: '/run/xraytun/daemon.sock',
    log_path: '/Users/x/Library/Logs/XrayTun/xt-daemon.log',
    ...overrides,
  };
}

describe('launchReportText', () => {
  it('没有报告 ⇒ 一个字都不说（不编「启动失败」）', () => {
    expect(launchReportText(null)).toBe(null);
  });

  it('unavailable：把壳给的原因原样带上', () => {
    expect(launchReportText(report())).toBe(
      '壳没能启动 daemon：没找到 xt-daemon：设 XT_DAEMON_BIN 指向它',
    );
  });

  it('unavailable 但壳没给原因 ⇒ 如实说「没有给出原因」，不猜', () => {
    expect(launchReportText(report({ reason: null }))).toBe('壳没能启动 daemon：（壳没有给出原因）');
    expect(launchReportText(report({ reason: '   ' }))).toBe(
      '壳没能启动 daemon：（壳没有给出原因）',
    );
  });

  it('not_listening：说的是「没能确认在监听」，不是「启动失败」', () => {
    const text = launchReportText(
      report({ outcome: 'not_listening', pid: 4242, reason: '等了 20 秒仍未收到就绪信号' }),
    );
    expect(text).toContain('没能确认它在监听');
    expect(text).toContain('等了 20 秒仍未收到就绪信号');
    expect(text).not.toContain('启动失败');
  });

  it('spawned / already_listening：照实说，没有坏消息', () => {
    expect(launchReportText(report({ outcome: 'spawned', reason: null, pid: 123 }))).toBe(
      '壳已确认 daemon 在监听（pid 123）',
    );
    expect(launchReportText(report({ outcome: 'already_listening', reason: null }))).toBe(
      '壳启动前就已有 daemon 在监听',
    );
  });

  it('壳将来加了新结论 ⇒ 原样带出来，不猜它的意思', () => {
    expect(
      launchReportText(report({ outcome: 'something_new' as LaunchReport['outcome'] })),
    ).toBe('壳报告：something_new');
  });
});

describe('launchLogLine', () => {
  it('有日志路径就给出来（用户要看的是一手现场）', () => {
    expect(launchLogLine(report())).toBe(
      'daemon 日志：/Users/x/Library/Logs/XrayTun/xt-daemon.log',
    );
  });

  it('壳没给路径 ⇒ 不编一个默认路径（环境变量可能把它指到别处）', () => {
    expect(launchLogLine(report({ log_path: null }))).toBe(null);
    expect(launchLogLine(report({ log_path: '' }))).toBe(null);
    expect(launchLogLine(null)).toBe(null);
  });
});
