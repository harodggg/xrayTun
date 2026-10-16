// @vitest-environment jsdom
//
// v1.2.3 · 「只做 TUN」的产品面验收（用户 2026-10-16 裁决：放弃 SOCKS 代理）。
//
// daemon 里那条 SOCKS 数据面还在（CI 唯一能证明「真有字节穿过隧道」的通路，TUN 要 root/助手
// 跑不了），但它**不许再出现在产品面上**。这个文件把「产品面只做 TUN」钉成 4 条断言：
//   1. 设置页不再有「代理入口」「SOCKS 监听地址」这类说法；
//   2. 那一栏说的是它现在真做的事（本机 API 监听基址），并且明说产品只做 TUN；
//   3. 能力行仍是 daemon 的原始声明（不许美化），但旁边解释 proxy_mode 是什么；
//   4. 那一栏仍然是可用的（保存真的把值发给 daemon）—— 不是把一个能用的一栏删成死控件。

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from '../../src/App';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type { DaemonHello, SettingsView } from '../../src/transport/contract';

const T0 = 1_700_000_000_000;

const HELLO: DaemonHello = {
  daemon_version: '0.0.0-test',
  protocol_version: 1,
  // 顺序与线上一致（daemon 声明什么就显示什么，界面不许重排成「好看」的样子）。
  capabilities: ['proxy_mode', 'tun_mode', 'stats', 'subscriptions', 'node_import'],
  pid: 4242,
  started_at_ms: T0,
};

const SETTINGS: SettingsView = { socks_listen: '127.0.0.1:1080', log_level: 'info' };

interface Rig {
  client: DaemonClient;
  patchCalls: Partial<SettingsView>[];
}

function makeClient(): Rig {
  const patchCalls: Partial<SettingsView>[] = [];
  const client: DaemonClient = {
    hello: () => Promise.resolve(HELLO),
    subscribe: () => Promise.resolve(),
    onEvent: () => () => {},
    status: () => Promise.resolve({ stage: 'disconnected', datapath: {} }),
    connect: () => Promise.resolve(),
    disconnect: () => Promise.resolve(),
    switchNode: () => Promise.resolve(),
    listNodes: () => Promise.resolve([]),
    probeNodes: () => Promise.resolve(),
    getSettings: () => Promise.resolve(SETTINGS),
    patchSettings: (patch) => {
      patchCalls.push(patch);
      return Promise.resolve();
    },
    listSubscriptions: () => Promise.resolve([]),
    addSubscription: () => Promise.resolve(),
    importNodes: () => Promise.resolve(),
    refreshSubscription: () => Promise.resolve(),
    tailLogs: () => Promise.resolve([]),
    close: () => {},
    onTransportError: () => () => {},
  };
  return { client, patchCalls };
}

async function openSettings(rig: Rig) {
  render(
    <DaemonProvider client={rig.client}>
      <App />
    </DaemonProvider>,
  );
  fireEvent.click(await screen.findByTestId('nav-settings'));
  return screen.findByTestId('page-settings');
}

beforeEach(() => {
  vi.restoreAllMocks();
});

afterEach(cleanup);

describe('产品面只做 TUN（SOCKS 代理不再是入口）', () => {
  it('设置页没有「代理入口」「SOCKS 监听地址」，那一栏说的是它真做的事', async () => {
    const rig = makeClient();
    await openSettings(rig);

    // 逐字判据：这两句话在 TUN-only 的产品里都是过期说法。用**整段文本相等**来判，
    // 而不是「页面上出现过这几个字」—— 新说明里有一句「这个地址不是代理入口」是正当的否定句。
    await screen.findByTestId('api-listen-card');
    expect(screen.queryByText('代理入口')).toBeNull();
    expect(screen.queryByText('SOCKS 监听地址')).toBeNull();

    expect(await screen.findByTestId('api-listen-card')).toBeTruthy();
    const hint = await screen.findByTestId('api-listen-hint');
    expect(hint.textContent).toContain('只做 TUN');
    expect(hint.textContent).toContain('不再提供 SOCKS 入站');
    expect(hint.textContent).toContain('本机 API');
  });

  it('能力行保持 daemon 的原始声明，但旁边解释 proxy_mode 是什么', async () => {
    const rig = makeClient();
    await openSettings(rig);

    // 不美化：线上是什么就显示什么（现有用例也钉着它）。
    expect(screen.getByTestId('daemon-capabilities').textContent).toBe(
      'proxy_mode、tun_mode、stats、subscriptions、node_import',
    );
    const note = await screen.findByTestId('capability-scope-note');
    expect(note.textContent).toContain('CI 端到端验证');
    expect(note.textContent).toContain('本产品的连接模式只有 TUN');
  });

  it('那一栏仍然可用：保存真的把值发给 daemon（不是删成死控件）', async () => {
    const rig = makeClient();
    await openSettings(rig);

    const input = (await screen.findByTestId('socks-listen-input')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: '127.0.0.1:1081' } });
    fireEvent.click(screen.getByTestId('save-socks-button'));

    await waitFor(() => expect(rig.patchCalls).toEqual([{ socks_listen: '127.0.0.1:1081' }]));
  });

  it('daemon 没宣告 proxy_mode 时，那句解释也不出现（不为不存在的事实写说明）', async () => {
    const rig = makeClient();
    rig.client.hello = () =>
      Promise.resolve({ ...HELLO, capabilities: ['tun_mode', 'stats'] });
    await openSettings(rig);

    await waitFor(() =>
      expect(screen.getByTestId('daemon-capabilities').textContent).toBe('tun_mode、stats'),
    );
    expect(screen.queryByTestId('capability-scope-note')).toBeNull();
  });
});
