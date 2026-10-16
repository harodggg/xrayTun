// @vitest-environment jsdom
//
// v1.2.0 · 特权助手安装验收（死路 D4：TUN 需要的 helper 从来没人装，界面上也没有任何
// 地方能修 —— 只有一条 `helper_unavailable`）。
//
// 这个文件证两件事：
//   1. 设置页第一张卡片由壳注入的全局标记门控：标记缺失时整块不出现（不摆假控件）；
//      标记在时，四个客观事实各自映射到一行状态，且安装 / 重启 / 卸载都真的打到壳命令上；
//   2. 仪表板收到 `helper_unavailable` 且壳能装 helper 时，给出「去安装特权助手」这颗
//      真正的下一步按钮，点它落到设置页的卡片上。
//
// 壳的 invoke 用**本文件自己的** vi.mock 顶替；这些数据是测试数据，不是运行时数据源。

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../../src/App';
import type { HelperStatus } from '../../src/shell';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type { ConnectionView, DaemonHello, SettingsView } from '../../src/transport/contract';

const shellMock = vi.hoisted(() => {
  const calls: string[] = [];
  const state = {
    status: {
      installed: true,
      plist_present: true,
      socket_present: true,
      reachable: true,
      binary_in_bundle: true,
    } as HelperStatus,
    installReject: null as string | null,
  };
  return { calls, state };
});

// 与 shell.ts 的动态 import 对接：命令名逐个记录，helper_status 返回当前事实，
// install_helper 在需要时按壳的形状 reject 一个**字符串**。
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (command: string) => {
    shellMock.calls.push(command);
    if (command === 'helper_status') {
      return Promise.resolve({ ...shellMock.state.status });
    }
    if (command === 'install_helper' && shellMock.state.installReject !== null) {
      return Promise.reject(shellMock.state.installReject);
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

const DISCONNECTED: ConnectionView = { stage: 'disconnected', datapath: {} };

const DEFAULT_STATUS: HelperStatus = {
  installed: true,
  plist_present: true,
  socket_present: true,
  reachable: true,
  binary_in_bundle: true,
};

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

function renderApp(connection: ConnectionView = DISCONNECTED) {
  return render(
    <DaemonProvider client={makeClient(connection)}>
      <App />
    </DaemonProvider>,
  );
}

/** 打开设置页并等到第一张卡片出现。 */
async function openSettings(): Promise<HTMLElement> {
  fireEvent.click(await screen.findByTestId('nav-settings'));
  return screen.findByTestId('helper-card');
}

beforeEach(() => {
  shellMock.calls.length = 0;
  shellMock.state.status = { ...DEFAULT_STATUS };
  shellMock.state.installReject = null;
  window.__XT_HELPER_INSTALL__ = true;
});

afterEach(() => {
  cleanup();
  delete window.__XT_HELPER_INSTALL__;
});

describe('v1.2.0 · 特权助手卡片由壳标记门控', () => {
  it('壳没注入标记 ⇒ helper-card 整块不出现，也不发任何壳命令', async () => {
    delete window.__XT_HELPER_INSTALL__;
    renderApp();
    fireEvent.click(await screen.findByTestId('nav-settings'));
    await screen.findByTestId('page-settings');

    expect(screen.queryByTestId('helper-card')).toBeNull();
    expect(shellMock.calls).toEqual([]);
  });

  it('四个事实全为真 ⇒ 「已安装 · 正在服务」+ 细节行', async () => {
    renderApp();
    await openSettings();

    expect((await screen.findByTestId('helper-status')).textContent).toBe('已安装 · 正在服务');
    expect((await screen.findByTestId('helper-detail')).textContent).toBe(
      '二进制：已装 · launchd 配置：在 · socket：在',
    );
  });

  it('installed 为真但 reachable 为假 ⇒ 「已安装，但连不上」+ 重启按钮', async () => {
    shellMock.state.status.reachable = false;
    renderApp();
    await openSettings();

    expect((await screen.findByTestId('helper-status')).textContent).toBe(
      '已安装，但连不上（socket 没有在服务）',
    );
    expect((await screen.findByTestId('helper-restart-button')).textContent).toBe('重启助手');
    expect(screen.queryByTestId('helper-install-button')).toBeNull();
  });

  it('未安装 ⇒ 安装按钮；点它真的调 install_helper 并重新拉 status', async () => {
    shellMock.state.status = {
      installed: false,
      plist_present: false,
      socket_present: false,
      reachable: false,
      binary_in_bundle: true,
    };
    renderApp();
    await openSettings();

    expect((await screen.findByTestId('helper-status')).textContent).toBe('未安装 —— TUN 模式无法使用');
    fireEvent.click(await screen.findByTestId('helper-install-button'));

    await waitFor(() =>
      expect(shellMock.calls).toEqual(['helper_status', 'install_helper', 'helper_status']),
    );
  });

  it('卸载必须两次点击：第一次只进确认态，第二次才调 uninstall_helper', async () => {
    renderApp();
    await openSettings();

    fireEvent.click(await screen.findByTestId('helper-uninstall-button'));
    // 第一次点击绝不许真的卸载。
    expect(shellMock.calls).toEqual(['helper_status']);
    expect(screen.queryByTestId('helper-uninstall-button')).toBeNull();

    const confirm = await screen.findByTestId('helper-uninstall-confirm');
    expect(confirm.textContent).toBe('确认卸载？');
    fireEvent.click(confirm);

    await waitFor(() => expect(shellMock.calls).toContain('uninstall_helper'));
  });

  it('安装失败 ⇒ helper-error 逐字显示壳给的字符串', async () => {
    shellMock.state.status = {
      installed: false,
      plist_present: false,
      socket_present: false,
      reachable: false,
      binary_in_bundle: true,
    };
    shellMock.state.installReject = '管理员授权被取消（或系统未给出原因）';
    renderApp();
    await openSettings();

    fireEvent.click(await screen.findByTestId('helper-install-button'));

    const box = await screen.findByTestId('helper-error');
    expect(box.textContent).toContain('管理员授权被取消（或系统未给出原因）');
  });
});

describe('v1.2.0 · 仪表板上的 helper_unavailable 有下一步', () => {
  const HELPER_FAILED: ConnectionView = {
    stage: 'disconnected',
    datapath: {},
    last_error: { code: 'helper_unavailable', message: '连接 helper socket …' },
  };

  it('壳能装 ⇒ CTA 存在，点它落到设置页的卡片上', async () => {
    renderApp(HELPER_FAILED);

    const cta = await screen.findByTestId('helper-install-cta');
    expect(cta.textContent).toBe('去安装特权助手');
    expect((await screen.findByTestId('last-error')).textContent).toContain('helper_unavailable');

    fireEvent.click(cta);
    await screen.findByTestId('helper-card');
  });

  it('壳装不了 ⇒ 不给 CTA，保持只有查看日志的现状', async () => {
    delete window.__XT_HELPER_INSTALL__;
    renderApp(HELPER_FAILED);

    await screen.findByTestId('last-error');
    expect(screen.queryByTestId('helper-install-cta')).toBeNull();
  });
});
