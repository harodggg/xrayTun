// @vitest-environment jsdom
//
// v1.2.0 仪表板验收（设计 docs/design/NEXT-VERSION-UI.md §3.2 / §4 / §7）。
//
// 为什么单独一个文件：truthfulness.test.tsx 证的是「每个显示项都能追回契约字段」；
// 这里证的是**这一版新增或改变的行为**：
//   * 大启动按钮是全屏唯一的主控，模式选择器彻底消失（产品面只留 TUN）；
//   * 顶栏与仪表板的状态词同源（同一事实不许两处判据）；
//   * 网速必须由**两个不同 sampled_at_ms 的样本**派生，断开后基准清空；
//   * Event::Notice 被真正渲染（原文逐字，不改写）；
//   * 源码里不存在意图 / 拓扑 / 地球仪页面（裁决 2）。

import { readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { App } from '../../src/App';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type {
  Capability,
  ConnectionView,
  DaemonEvent,
  DaemonHello,
  NodeView,
  SettingsView,
} from '../../src/transport/contract';

afterEach(cleanup);

const T0 = 1_700_000_000_000;
const CAPS: Capability[] = ['proxy_mode', 'tun_mode', 'stats', 'probe', 'subscriptions'];

const HELLO: DaemonHello = {
  daemon_version: '0.0.0-test',
  protocol_version: 1,
  capabilities: CAPS,
  pid: 4242,
  started_at_ms: T0,
};

const NODE: NodeView = {
  id: 'node-1',
  name: '测试节点一',
  protocol: 'vless',
  endpoint: '127.0.0.1:443',
  source: { kind: 'manual' },
};

const SETTINGS: SettingsView = { socks_listen: '127.0.0.1:1080', selected_node: 'node-1', log_level: 'info' };

const DISCONNECTED: ConnectionView = { stage: 'disconnected', datapath: {} };

interface Rig {
  client: DaemonClient;
  emit: (event: DaemonEvent) => void;
}

function makeRig(): Rig {
  const handlers = new Set<(event: DaemonEvent) => void>();
  const client: DaemonClient = {
    hello: () => Promise.resolve(HELLO),
    subscribe: () => Promise.resolve(),
    onEvent: (handler) => {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
    status: () => Promise.resolve(DISCONNECTED),
    connect: () => Promise.resolve(),
    disconnect: () => Promise.resolve(),
    switchNode: () => Promise.resolve(),
    listNodes: () => Promise.resolve([NODE]),
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
  return {
    client,
    emit: (event) => {
      for (const handler of [...handlers]) handler(event);
    },
  };
}

function renderApp(rig: Rig) {
  return render(
    <DaemonProvider client={rig.client}>
      <App />
    </DaemonProvider>,
  );
}

const state = (view: ConnectionView): DaemonEvent => ({ event: 'state', view });

describe('v1.2.0 · 大启动按钮是唯一主控，没有模式选择器', () => {
  it('start-button 在位；旧的连接/断开/模式选择器一个都不在', async () => {
    const rig = makeRig();
    renderApp(rig);

    const button = (await screen.findByTestId('start-button')) as HTMLButtonElement;
    expect(button.textContent).toBe('启动');
    expect(button.disabled).toBe(false);
    expect(screen.queryByTestId('connect-button')).toBeNull();
    expect(screen.queryByTestId('disconnect-button')).toBeNull();
    expect(screen.queryByTestId('run-mode-select')).toBeNull();
  });

  it('点一下就发 tun 意图（产品面只留 TUN）', async () => {
    const rig = makeRig();
    const calls: string[] = [];
    rig.client.connect = (nodeId, mode) => {
      calls.push(`${nodeId}/${mode}`);
      return Promise.resolve();
    };
    renderApp(rig);

    fireEvent.click(await screen.findByTestId('start-button'));
    await waitFor(() => expect(calls).toEqual(['node-1/tun']));
  });
});

describe('v1.2.0 · 顶栏与仪表板状态词同源', () => {
  it('未连接 / 已连接 两档，两处逐字一致', async () => {
    const rig = makeRig();
    renderApp(rig);

    await fireEvent.click(await screen.findByTestId('nav-dashboard'));
    expect((await screen.findByTestId('dash-status-label')).textContent).toBe('未连接');
    expect(screen.getByTestId('topbar-status').textContent).toBe('未连接');

    rig.emit(
      state({
        stage: 'connected',
        mode: 'tun',
        node_id: 'node-1',
        connected_since_ms: T0,
        datapath: { pid: 7 },
      }),
    );
    await waitFor(() =>
      expect(screen.getByTestId('dash-status-label').textContent).toBe('已连接'),
    );
    expect(screen.getByTestId('topbar-status').textContent).toBe('已连接');
    // 只连接后才会显示「已运行」与模式；两者都来自契约字段。
    expect(screen.getByTestId('dash-meta').textContent).toContain('TUN 模式');
    expect(screen.getByTestId('dash-meta').textContent).toContain('已运行');
  });
});

describe('v1.2.0 · 网速必须两个样本才派生，断开清空基准', () => {
  it('一个样本不显示速率；两个样本算出速率；断开后回到未采样', async () => {
    const rig = makeRig();
    renderApp(rig);
    await screen.findByTestId('nav-dashboard');

    rig.emit(
      state({
        stage: 'connected',
        datapath: { pid: 7 },
        stats: { uplink_bytes: 1000, downlink_bytes: 1000, sampled_at_ms: 1000 },
      }),
    );
    // 首个样本：累计字节有了，速率仍必须说「未采样」。
    await screen.findByTestId('stats-uplink-total');
    expect(screen.getByTestId('stats-uplink-rate').textContent).toBe('未采样');

    rig.emit(
      state({
        stage: 'connected',
        datapath: { pid: 7 },
        stats: { uplink_bytes: 3000, downlink_bytes: 3000, sampled_at_ms: 2000 },
      }),
    );
    // Δ2000B / Δ1s = 2000 B/s → formatBytes(2000) = 2.0 KiB
    await waitFor(() =>
      expect(screen.getByTestId('stats-uplink-rate').textContent).toBe('2.0 KiB/s'),
    );

    // 断开：基准清空 —— 再连上时只有一个新样本，不许拿跨会话的两个数字相减。
    rig.emit(state({ stage: 'disconnected', datapath: {} }));
    await waitFor(() => expect(screen.queryByTestId('stats-uplink-rate')).toBeNull());

    rig.emit(
      state({
        stage: 'connected',
        datapath: { pid: 8 },
        stats: { uplink_bytes: 999_999, downlink_bytes: 999_999, sampled_at_ms: 9000 },
      }),
    );
    await screen.findByTestId('stats-uplink-total');
    expect(screen.getByTestId('stats-uplink-rate').textContent).toBe('未采样');
  });
});

describe('v1.2.0 · Event::Notice 被真正渲染', () => {
  it('notice 原文逐字出现在横幅里；没有 notice 就没有横幅', async () => {
    const rig = makeRig();
    renderApp(rig);
    await screen.findByTestId('nav-dashboard');
    expect(screen.queryByTestId('dash-banner')).toBeNull();

    const MESSAGE = '核心进程在就绪前退出：exit status 1（notice 原文，逐字断言）';
    rig.emit({ event: 'notice', notice: { severity: 'error', code: 'core_exited_early', message: MESSAGE, at_ms: T0 } });

    const banner = await screen.findByTestId('dash-banner');
    expect(banner.textContent).toContain(MESSAGE);
    expect(banner.getAttribute('role')).toBe('alert');
  });
});

describe('v1.2.0 · 源码里没有意图 / 拓扑 / 地球仪页面', () => {
  it('apps/ui/src 下不存在以 intent / topology / globe 命名的文件', () => {
    const here = path.dirname(fileURLToPath(import.meta.url));
    const src = path.resolve(here, '../../src');
    const files = readdirSync(src, { recursive: true }) as string[];
    const banned = files.filter((name) => /intent|topolog|globe/i.test(String(name)));
    expect(banned).toEqual([]);
  });
});
