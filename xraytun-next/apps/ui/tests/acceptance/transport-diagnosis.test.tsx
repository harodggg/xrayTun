// @vitest-environment jsdom
//
// v1.2.1 · 连不上 daemon 时的「为什么」验收。
//
// 真人实测里最容易卡住的一屏是：App 打开了，界面只有一句「连接 daemon … 失败」，
// 下面一颗「重新连接」，谁也不知道 daemon 为什么没起来。壳其实**知道**（没找到二进制 /
// spawn 被拒 / 报就绪前就退了），以前那知识只进了 `eprintln`。
//
// 这个文件证四件事：
//   1. 壳注入了标记 + 拿到报告 + 传输错误码是 `io` ⇒ 页面出现壳的结论与 daemon 日志路径；
//   2. 壳**没**注入标记 ⇒ 一个字都不显示，且一次壳命令都不发（无头环境不许去问）；
//   3. 壳**没给**报告（null）⇒ 不显示，也**不**编一句「启动失败」；
//   4. 传输错误不是 `io`（例如事件流内部的错误）⇒ 不把用户引到壳启动那条线索上。
//
// 壳的 invoke 用**本文件自己的** vi.mock 顶替；报告内容是测试数据，不是运行时数据源。

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../../src/App';
import type { LaunchReport } from '../../src/shell';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type { ConnectionView, ErrorBody, SettingsView } from '../../src/transport/contract';

const shellMock = vi.hoisted(() => {
  const calls: string[] = [];
  const state = { report: null as LaunchReport | null };
  return { calls, state };
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string) => {
    shellMock.calls.push(command);
    if (command === 'daemon_launch_report') return Promise.resolve(shellMock.state.report);
    return Promise.resolve(undefined);
  },
}));

const SETTINGS: SettingsView = { socks_listen: '127.0.0.1:1080', log_level: 'info' };

const DISCONNECTED: ConnectionView = { stage: 'disconnected', datapath: {} };

const CONNECT_FAILURE: ErrorBody = {
  code: 'io',
  message: '连接 daemon /run/xraytun/daemon.sock 失败：No such file or directory（测试数据）',
};

const UNAVAILABLE: LaunchReport = {
  outcome: 'unavailable',
  reason: '没找到 xt-daemon：设 XT_DAEMON_BIN 指向它（测试数据）',
  pid: null,
  bin: null,
  socket: '/run/xraytun/daemon.sock',
  log_path: '/Users/x/Library/Logs/XrayTun/xt-daemon.log',
};

/** 引导链第一步就失败的 client：这就是「连不上 daemon」在界面上的来源。 */
function makeFailingClient(error: ErrorBody): DaemonClient {
  return {
    hello: () => Promise.reject(error),
    subscribe: () => Promise.resolve(),
    onEvent: () => () => {},
    status: () => Promise.resolve(DISCONNECTED),
    connect: () => Promise.resolve(),
    disconnect: () => Promise.resolve(),
    switchNode: () => Promise.resolve(),
    listNodes: () => Promise.resolve([]),
    probeNodes: () => Promise.resolve(),
    getSettings: () => Promise.resolve(SETTINGS),
    patchSettings: () => Promise.resolve(),
    listSubscriptions: () => Promise.resolve([]),
    addSubscription: () => Promise.resolve(),
    importNodes: () => Promise.resolve(),
    refreshSubscription: () => Promise.resolve(),
    tailLogs: () => Promise.resolve([]),
    close: () => {},
    onTransportError: () => () => {},
  };
}

function renderApp(error: ErrorBody) {
  return render(
    <DaemonProvider client={makeFailingClient(error)}>
      <App />
    </DaemonProvider>,
  );
}

beforeEach(() => {
  shellMock.calls.length = 0;
  shellMock.state.report = { ...UNAVAILABLE };
  window.__XT_LAUNCH_REPORT__ = true;
});

afterEach(() => {
  cleanup();
  delete window.__XT_LAUNCH_REPORT__;
});

describe('连不上 daemon 时给出壳的启动结论', () => {
  it('壳报告「没能启动」⇒ 页面出现原因 + daemon 日志路径，且只问壳一次', async () => {
    renderApp(CONNECT_FAILURE);

    // 传输错误仍然原样显示（不因为多了诊断就把它藏起来）。
    const box = await screen.findByTestId('transport-error');
    expect(box.textContent).toContain('连接 daemon /run/xraytun/daemon.sock 失败');

    const diagnosis = await screen.findByTestId('launch-report');
    expect(diagnosis.textContent).toContain('壳没能启动 daemon：没找到 xt-daemon：设 XT_DAEMON_BIN 指向它');
    expect(diagnosis.textContent).toContain(
      'daemon 日志：/Users/x/Library/Logs/XrayTun/xt-daemon.log',
    );

    // 只问一次：这是「上次启动的结论」，不是需要盯着看的状态（不轮询、不重试）。
    await waitFor(() =>
      expect(shellMock.calls.filter((name) => name === 'daemon_launch_report')).toHaveLength(1),
    );
  });

  it('壳没注入标记（无头 / 浏览器）⇒ 不显示诊断，也一次壳命令都不发', async () => {
    delete window.__XT_LAUNCH_REPORT__;
    renderApp(CONNECT_FAILURE);

    await screen.findByTestId('transport-error');
    expect(screen.queryByTestId('launch-report')).toBeNull();
    expect(shellMock.calls).toEqual([]);
  });

  it('壳没给报告（null）⇒ 不显示，也不编一句「启动失败」', async () => {
    shellMock.state.report = null;
    renderApp(CONNECT_FAILURE);

    await screen.findByTestId('transport-error');
    expect(screen.queryByTestId('launch-report')).toBeNull();
  });

  it('传输错误不是 io ⇒ 不把用户引到「壳启动 daemon」那条线索上', async () => {
    renderApp({
      code: 'internal',
      message: '事件流跳号：期望 seq 7，收到 9（测试数据）',
    });

    await screen.findByTestId('transport-error');
    await waitFor(() => expect(shellMock.calls).toEqual([]));
    expect(screen.queryByTestId('launch-report')).toBeNull();
  });
});
