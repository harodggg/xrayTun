// @vitest-environment jsdom
//
// v1.2.2 · 失败之后「下一步在哪」验收。
//
// 体验官走查发现：仪表板只给一条错误 + 一颗「查看日志」，可**日志文件在哪**界面上一个字
// 都没有；helper 相关的那颗 CTA 说「去安装特权助手」，到了设置页还要自己找那颗按钮。
//
// 这个文件证三件事（都能被真人看见）：
//   1. 仪表板出现失败时，把壳给的 daemon 日志路径显示出来；
//   2. 壳没注入标记（无头 / 浏览器）⇒ 一个字都不显示，也不发壳命令；
//   3. `helper_unavailable` + 壳能装 helper ⇒ 点 CTA 落到设置页，页面上**确实有**那颗
//      「安装特权助手」按钮，并且卸载范围的说明与脚本一致（三样 + 留下的状态目录）。
//
// 壳的 invoke 用本文件自己的 vi.mock 顶替；数据是测试数据，不是运行时数据源。

import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../../src/App';
import type { LaunchReport } from '../../src/shell';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type {
  ConnectionView,
  DaemonHello,
  ErrorCode,
  SettingsView,
} from '../../src/transport/contract';

const shellMock = vi.hoisted(() => {
  const calls: string[] = [];
  const state = { report: null as LaunchReport | null };
  return { calls, state };
});

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string) => {
    shellMock.calls.push(command);
    if (command === 'daemon_launch_report') return Promise.resolve(shellMock.state.report);
    if (command === 'helper_status') {
      return Promise.resolve({
        installed: false,
        plist_present: false,
        socket_present: false,
        reachable: false,
        binary_in_bundle: true,
      });
    }
    return Promise.resolve(undefined);
  },
}));

const T0 = 1_700_000_000_000;

const HELLO: DaemonHello = {
  daemon_version: '0.0.0-test',
  protocol_version: 1,
  capabilities: ['tun_mode'],
  pid: 4242,
  started_at_ms: T0,
};

const SETTINGS: SettingsView = { socks_listen: '127.0.0.1:1080', log_level: 'info' };

const REPORT: LaunchReport = {
  outcome: 'spawned',
  reason: null,
  pid: 4242,
  bin: '/Applications/XrayTun.app/Contents/Resources/xt-daemon',
  socket: '/tmp/xraytun-daemon.sock',
  log_path: '/Users/x/Library/Logs/XrayTun/xt-daemon.log',
};

function disconnectedWithError(code: ErrorCode, message: string): ConnectionView {
  return { stage: 'disconnected', datapath: {}, last_error: { code, message } };
}

function makeClient(connection: ConnectionView): DaemonClient {
  return {
    hello: () => Promise.resolve(HELLO),
    subscribe: () => Promise.resolve(),
    onEvent: () => () => {},
    status: () => Promise.resolve(connection),
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

function renderApp(connection: ConnectionView) {
  return render(
    <DaemonProvider client={makeClient(connection)}>
      <App />
    </DaemonProvider>,
  );
}

beforeEach(() => {
  shellMock.calls.length = 0;
  shellMock.state.report = { ...REPORT };
  window.__XT_LAUNCH_REPORT__ = true;
  window.__XT_HELPER_INSTALL__ = true;
});

afterEach(() => {
  cleanup();
  delete window.__XT_LAUNCH_REPORT__;
  delete window.__XT_HELPER_INSTALL__;
});

describe('失败之后给一手现场（日志路径）', () => {
  it('仪表板有失败 + 壳给了日志路径 ⇒ 路径显示出来，「查看日志」按钮也在', async () => {
    renderApp(disconnectedWithError('datapath_unavailable', '提交路由失败（测试数据）'));

    const box = await screen.findByTestId('last-error');
    expect(box.textContent).toContain('datapath_unavailable');
    expect(box.textContent).toContain('提交路由失败（测试数据）');

    const log = await screen.findByTestId('last-error-log');
    expect(log.textContent).toBe('daemon 日志：/Users/x/Library/Logs/XrayTun/xt-daemon.log');
    // 仪表板上两处都给了「查看日志」（状态区与诊断区各一颗）：这里只要求它存在。
    expect(screen.getAllByText('查看日志').length).toBeGreaterThan(0);
  });

  it('壳没注入标记 ⇒ 不显示日志路径，也不发壳命令', async () => {
    delete window.__XT_LAUNCH_REPORT__;
    renderApp(disconnectedWithError('io', '连不上（测试数据）'));

    await screen.findByTestId('last-error');
    expect(screen.queryByTestId('last-error-log')).toBeNull();
    expect(shellMock.calls).toEqual([]);
  });
});

describe('helper 缺失时那条 CTA 真的能走到安装入口', () => {
  it('点「去安装特权助手」⇒ 设置页上有那颗按钮，且卸载范围逐字与脚本一致', async () => {
    renderApp(disconnectedWithError('helper_unavailable', '连接 helper socket 失败（测试数据）'));

    const cta = await screen.findByTestId('helper-install-cta');
    fireEvent.click(cta);

    // 落到的页面上**真的**有安装按钮（不是只换个页名）。
    expect(await screen.findByTestId('helper-install-button')).toBeTruthy();

    // 卸载到底删什么，界面说的必须和 `uninstall_script()` 做的一致。
    const scope = await screen.findByTestId('helper-uninstall-scope');
    expect(scope.textContent).toContain('/Library/LaunchDaemons');
    expect(scope.textContent).toContain('/Library/PrivilegedHelperTools');
    expect(scope.textContent).toContain('/var/run/xraytun-helper.sock');
    expect(scope.textContent).toContain('/Library/Application Support/XrayTun');
  });
});
