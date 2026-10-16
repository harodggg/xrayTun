// @vitest-environment jsdom
//
// v1.2.0 · 节点导入验收（用户实测结论：已发布版本界面上没有任何地方能导入节点）。
//
// 这个文件只证一件事：**用户真的能把节点弄进来**，且整条路径都由
// `DaemonHello.capabilities` 里的 `node_import` 门控 —— 没宣告就一个入口都不渲染。
//
// 用**本文件自己的**最小 fake client 注入契约数据（不从别的测试文件 import：
// 验收之间的耦合会让「改了 A 的 fake」悄悄改掉 B 的结论）。
// 与 truthfulness.test.tsx 一样：这里的数据是**测试数据**，不是运行时数据源。

import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { App } from '../../src/App';
import { DaemonProvider } from '../../src/store/daemon';
import type { DaemonClient } from '../../src/transport/client';
import type {
  Capability,
  ConnectionView,
  DaemonHello,
  ErrorBody,
  NodeView,
  SettingsView,
} from '../../src/transport/contract';

afterEach(cleanup);

const T0 = 1_700_000_000_000;

function helloOf(capabilities: Capability[]): DaemonHello {
  return {
    daemon_version: '0.0.0-test',
    protocol_version: 1,
    capabilities,
    pid: 4242,
    started_at_ms: T0,
  };
}

const DISCONNECTED: ConnectionView = { stage: 'disconnected', datapath: {} };

const SETTINGS: SettingsView = { socks_listen: '127.0.0.1:1080', log_level: 'info' };

const NODE_ONE: NodeView = {
  id: 'node-1',
  name: '测试节点一',
  protocol: 'vless',
  endpoint: '127.0.0.1:443',
  source: { kind: 'manual' },
};

interface Rig {
  client: DaemonClient;
  /** 每次 importNodes 收到的原文，逐字记录。 */
  importCalls: string[];
}

function makeClient(options: {
  capabilities: Capability[];
  nodes?: NodeView[];
  importError?: ErrorBody;
  /** daemon 里已选中的节点（不传 = 还没选）。 */
  selectedNode?: string;
}): Rig {
  const importCalls: string[] = [];
  const settings: SettingsView =
    options.selectedNode === undefined
      ? SETTINGS
      : { ...SETTINGS, selected_node: options.selectedNode };
  const client: DaemonClient = {
    hello: () => Promise.resolve(helloOf(options.capabilities)),
    subscribe: () => Promise.resolve(),
    onEvent: () => () => {},
    status: () => Promise.resolve(DISCONNECTED),
    connect: () => Promise.resolve(),
    disconnect: () => Promise.resolve(),
    switchNode: () => Promise.resolve(),
    listNodes: () => Promise.resolve(options.nodes ?? []),
    probeNodes: () => Promise.resolve(),
    getSettings: () => Promise.resolve(settings),
    patchSettings: () => Promise.resolve(),
    listSubscriptions: () => Promise.resolve([]),
    addSubscription: () => Promise.resolve(),
    importNodes: (payload) => {
      importCalls.push(payload);
      return options.importError === undefined
        ? Promise.resolve()
        : Promise.reject(options.importError);
    },
    refreshSubscription: () => Promise.resolve(),
    tailLogs: () => Promise.resolve([]),
    close: () => {},
    onTransportError: () => () => {},
  };
  return { client, importCalls };
}

function renderApp(client: DaemonClient) {
  return render(
    <DaemonProvider client={client}>
      <App />
    </DaemonProvider>,
  );
}

/** 进入节点页并等导入区出现（capabilities 含 node_import 时）。 */
async function openNodesPage(): Promise<void> {
  fireEvent.click(await screen.findByTestId('nav-nodes'));
}

describe('节点导入入口由 node_import 能力门控', () => {
  it('含 node_import → 导入区在位；输入为空时按钮不可点', async () => {
    const rig = makeClient({ capabilities: ['proxy_mode', 'tun_mode', 'node_import'] });
    renderApp(rig.client);
    await openNodesPage();

    expect(await screen.findByTestId('import-nodes')).toBeTruthy();
    const input = screen.getByTestId('import-nodes-input') as HTMLTextAreaElement;
    const button = screen.getByTestId('import-nodes-button') as HTMLButtonElement;
    expect(input.value).toBe('');
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe('导入');
  });

  it('不含 node_import → 导入区不存在，空态如实说明本页无法导入', async () => {
    const rig = makeClient({ capabilities: ['proxy_mode', 'tun_mode', 'stats'], nodes: [] });
    renderApp(rig.client);

    // 先在设置页等 hello 真落地，避免把「能力还没读到」当成「没有宣告」。
    fireEvent.click(await screen.findByTestId('nav-settings'));
    await waitFor(() =>
      expect(screen.getByTestId('daemon-capabilities').textContent).toBe('proxy_mode、tun_mode、stats'),
    );

    await openNodesPage();
    expect(await screen.findByTestId('nodes-empty')).toBeTruthy();
    expect(screen.queryByTestId('import-nodes')).toBeNull();
    expect(screen.queryByTestId('import-nodes-input')).toBeNull();
    expect(screen.queryByTestId('import-nodes-button')).toBeNull();
    expect(screen.getByText('当前 daemon 没有宣告 node_import 能力，本页无法导入。')).toBeTruthy();
  });
});

describe('导入请求与结果如实落到界面', () => {
  it('点「导入」→ daemon 收到完全相同的原文；成功后输入框清空', async () => {
    const rig = makeClient({ capabilities: ['tun_mode', 'node_import'] });
    renderApp(rig.client);
    await openNodesPage();

    const payload = 'vless://b831381d-6324-4d53-ad4f-8cda48b30811@example.com:443#测试节点';
    const input = (await screen.findByTestId('import-nodes-input')) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: payload } });

    const button = screen.getByTestId('import-nodes-button') as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    fireEvent.click(button);

    await waitFor(() => expect(rig.importCalls).toEqual([payload]));
    await waitFor(() => expect(input.value).toBe(''));
  });

  it('首尾空白被裁掉后再发（发出去的不是「用户没看到的空格」）', async () => {
    const rig = makeClient({ capabilities: ['tun_mode', 'node_import'] });
    renderApp(rig.client);
    await openNodesPage();

    const input = (await screen.findByTestId('import-nodes-input')) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '  vless://abc@example.com:443  ' } });
    fireEvent.click(screen.getByTestId('import-nodes-button'));

    await waitFor(() => expect(rig.importCalls).toEqual(['vless://abc@example.com:443']));
  });

  it('daemon 拒绝 → code 与 message 原样出现在 nodes-action-error，输入框不清空', async () => {
    const rejection: ErrorBody = {
      code: 'invalid_request',
      message: '订阅正文为空，没有可解析的节点',
    };
    const rig = makeClient({ capabilities: ['tun_mode', 'node_import'], importError: rejection });
    renderApp(rig.client);
    await openNodesPage();

    const input = (await screen.findByTestId('import-nodes-input')) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '这不是订阅' } });
    fireEvent.click(screen.getByTestId('import-nodes-button'));

    const box = await screen.findByTestId('nodes-action-error');
    expect(box.textContent).toContain('invalid_request');
    expect(box.textContent).toContain('订阅正文为空，没有可解析的节点');
    // 失败时原文必须留着：清空会让用户重贴一遍。
    expect(input.value).toBe('这不是订阅');
  });
});

describe('无节点时的大启动按钮指向真实存在的导入入口', () => {
  it('文案是「导入节点」；点它跳到节点页', async () => {
    const rig = makeClient({ capabilities: ['tun_mode', 'node_import'], nodes: [] });
    renderApp(rig.client);

    const button = await screen.findByTestId('start-button');
    await waitFor(() => expect(button.textContent).toBe('导入节点'));
    expect((button as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(button);
    expect(await screen.findByTestId('page-nodes')).toBeTruthy();
    expect(await screen.findByTestId('import-nodes')).toBeTruthy();
  });

  it('目录非空但还没选节点：大按钮是「选择节点」，点了真跳到节点页（不是灰着不说去哪）', async () => {
    const rig = makeClient({ capabilities: ['tun_mode', 'node_import'], nodes: [NODE_ONE] });
    renderApp(rig.client);

    const button = await screen.findByTestId('start-button');
    await waitFor(() => expect(button.textContent).toBe('选择节点'));
    expect((button as HTMLButtonElement).disabled).toBe(false);
    // 说明里必须写清「到了节点页点哪一颗」——这是真人卡住的地方。
    expect(screen.getByTestId('start-button-note').textContent).toContain('选为当前节点');

    fireEvent.click(button);
    expect(await screen.findByTestId('page-nodes')).toBeTruthy();
  });

  it('选好节点之后才是「启动」：点了停在仪表盘，不会跳页', async () => {
    const rig = makeClient({
      capabilities: ['tun_mode', 'node_import'],
      nodes: [NODE_ONE],
      selectedNode: NODE_ONE.id,
    });
    renderApp(rig.client);

    const button = await screen.findByTestId('start-button');
    await waitFor(() => expect(button.textContent).toBe('启动'));
    expect((button as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(button);
    // 启动是异步意图：点了以后按钮进入「启动中…」，但**不会**把用户带走。
    await waitFor(() => expect(screen.queryByTestId('page-nodes')).toBeNull());
  });
});
