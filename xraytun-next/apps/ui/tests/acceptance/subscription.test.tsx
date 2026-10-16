// @vitest-environment jsdom
//
// 订阅（添加 / 刷新）验收 —— 「真人在设置页添加/刷新订阅」这条用户路径。
//
// 这个文件只证一件事：**界面上的动作真的变成了契约请求，且结果如实落回界面**：
//   * 入口由 `DaemonHello.capabilities` 里的 `subscription_fetch` 门控 —— 没宣告就不渲染
//     一个按不动的输入框；
//   * 输入框里的完整 URL（含 query 里的 token）逐字进入 `addSubscription(url)`，
//     不是截断的、也不是被界面改写过的；
//   * 变更成功后 store 会再拉一次订阅列表（`listSubscriptions`），刷新结果渲染出
//     「节点数」这类用户真能看见的信息；
//   * 每条订阅的「刷新」把**该条的 id** 交给 `refreshSubscription(id)`；
//   * daemon 拒绝时，页面上出现可见的错误提示（Settings.tsx 真实的呈现方式是
//     `<ErrorBox testId="settings-error">`，且渲染在**订阅卡片内**（出错的地方），不是页面顶部、
//     不是 toast）——「没崩溃」不算通过。
//
// 用**本文件自己的**最小 fake client 注入契约数据（不从别的测试文件 import：
// 验收之间的耦合会让「改了 A 的 fake」悄悄改掉 B 的结论）。
// 与 node-import.test.tsx / truthfulness.test.tsx 一样：这里的数据是**测试数据**，
// 不是运行时数据源。

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
  SettingsView,
  SubscriptionView,
} from '../../src/transport/contract';

afterEach(cleanup);

const T0 = 1_700_000_000_000;

/** 用户在输入框里敲进去的原文：query 里的 token 属于 URL 本体，必须逐字送达。 */
const USER_URL = 'https://example.com/sub?token=SECRET';

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

const SUB_ONE: SubscriptionView = {
  id: 'sub-1',
  url: 'https://example.com/sub?token=SECRET',
  node_count: 3,
  fetched_at_ms: T0,
};

interface Rig {
  client: DaemonClient;
  /** 每次 addSubscription 收到的原文，逐字记录。 */
  addCalls: string[];
  /** 每次 refreshSubscription 收到的 id，逐字记录。 */
  refreshCalls: string[];
  /** 每次 listSubscriptions 的返回快照（长度 = 调用次数，用它证明「又拉了一次」）。 */
  listCalls: SubscriptionView[][];
}

function makeRig(options: {
  capabilities: Capability[];
  /** 初始订阅列表（bootstrap 那次 listSubscriptions 的返回）。 */
  subscriptions?: SubscriptionView[];
  /** addSubscription 成功后订阅列表变成这个（模拟 daemon 侧真的新增了一条）。 */
  afterAdd?: SubscriptionView[];
  /** addSubscription 以此 ErrorBody 拒绝（失败路径）。 */
  addError?: ErrorBody;
}): Rig {
  const addCalls: string[] = [];
  const refreshCalls: string[] = [];
  const listCalls: SubscriptionView[][] = [];
  let subscriptions = options.subscriptions ?? [];

  const client: DaemonClient = {
    hello: () => Promise.resolve(helloOf(options.capabilities)),
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
    listSubscriptions: () => {
      listCalls.push(subscriptions);
      return Promise.resolve(subscriptions);
    },
    addSubscription: (url) => {
      addCalls.push(url);
      if (options.addError !== undefined) return Promise.reject(options.addError);
      // 变更成功：列表换成 daemon 新增后的真实快照，等下一次 listSubscriptions 取走。
      if (options.afterAdd !== undefined) subscriptions = options.afterAdd;
      return Promise.resolve();
    },
    importNodes: () => Promise.resolve(),
    refreshSubscription: (id) => {
      refreshCalls.push(id);
      return Promise.resolve();
    },
    tailLogs: () => Promise.resolve([]),
    close: () => {},
    onTransportError: () => () => {},
  };
  return { client, addCalls, refreshCalls, listCalls };
}

function renderApp(client: DaemonClient) {
  return render(
    <DaemonProvider client={client}>
      <App />
    </DaemonProvider>,
  );
}

/** 进入设置页，并等能力字段真的渲染出来（避免把「hello 还没到」当成「没有宣告」）。 */
async function openSettingsPage(expectedCapabilities: string): Promise<void> {
  fireEvent.click(await screen.findByTestId('nav-settings'));
  await waitFor(() =>
    expect(screen.getByTestId('daemon-capabilities').textContent).toBe(expectedCapabilities),
  );
}

describe('订阅入口由 subscription_fetch 能力门控', () => {
  it('不含 subscription_fetch → 没有「添加订阅」输入框，也没有提交 / 刷新按钮', async () => {
    const rig = makeRig({
      capabilities: ['proxy_mode', 'subscriptions'],
      subscriptions: [SUB_ONE],
    });
    renderApp(rig.client);
    await openSettingsPage('proxy_mode、subscriptions');

    // 只读列表在（subscriptions 已宣告），但拉取入口一个都不许出现。
    expect(screen.getByTestId('subscriptions-section')).toBeTruthy();
    expect(screen.getByTestId('subscription-row')).toBeTruthy();
    expect(screen.queryByTestId('subscription-url-input')).toBeNull();
    expect(screen.queryByTestId('add-subscription-button')).toBeNull();
    expect(screen.queryByTestId('refresh-subscription-button')).toBeNull();
    // 按钮文案也不该以别的形状留在页面上。
    expect(screen.getByTestId('subscriptions-section').textContent).not.toContain('添加订阅');
  });
});

describe('添加订阅：界面动作 → 契约入参 → 列表刷新', () => {
  it('填入完整 URL 点「添加订阅」→ daemon 收到逐字相同的完整 URL；随后重拉列表并渲染节点数', async () => {
    const rig = makeRig({
      capabilities: ['proxy_mode', 'subscriptions', 'subscription_fetch'],
      subscriptions: [],
      afterAdd: [{ id: 'sub-1', url: USER_URL, node_count: 3, fetched_at_ms: T0 }],
    });
    renderApp(rig.client);
    await openSettingsPage('proxy_mode、subscriptions、subscription_fetch');

    // 等空态出现：说明 bootstrap 那次 listSubscriptions 已经落地，基线可数。
    expect(await screen.findByTestId('subscriptions-empty')).toBeTruthy();
    const bootstrapListCalls = rig.listCalls.length;

    const input = screen.getByTestId('subscription-url-input') as HTMLInputElement;
    const button = screen.getByTestId('add-subscription-button') as HTMLButtonElement;
    expect(button.textContent).toBe('添加订阅');
    expect(button.disabled).toBe(true);

    fireEvent.change(input, { target: { value: USER_URL } });
    await waitFor(() => expect(button.disabled).toBe(false));
    fireEvent.click(button);

    // 1) 契约入参 = 用户填的完整 URL（含 query 里的 token），不是被截断/改写过的版本。
    await waitFor(() => expect(rig.addCalls).toEqual([USER_URL]));
    // 2) 变更成功后 store 又拉了一次订阅列表（而不是停在旧快照上）。
    await waitFor(() => expect(rig.listCalls.length).toBeGreaterThan(bootstrapListCalls));
    // 3) 刷新后的列表内容渲染出用户能看到的信息：URL 与「节点数」。
    const row = await screen.findByTestId('subscription-row');
    expect(row.textContent).toContain(USER_URL);
    expect(screen.getByTestId('subscription-node-count').textContent).toBe('3 个节点');
    expect(screen.queryByTestId('subscriptions-empty')).toBeNull();
    // 成功后才清空草稿；失败时不清（见失败路径那条）。
    expect(input.value).toBe('');
  });
});

describe('刷新订阅：界面动作 → 契约入参', () => {
  it('点某条的「刷新」→ daemon 收到这一条的 id', async () => {
    const rig = makeRig({
      capabilities: ['proxy_mode', 'subscriptions', 'subscription_fetch'],
      subscriptions: [SUB_ONE],
    });
    renderApp(rig.client);
    await openSettingsPage('proxy_mode、subscriptions、subscription_fetch');

    const button = (await screen.findByTestId('refresh-subscription-button')) as HTMLButtonElement;
    expect(button.textContent).toBe('刷新');
    fireEvent.click(button);

    await waitFor(() => expect(rig.refreshCalls).toEqual([SUB_ONE.id]));
  });
});

describe('添加订阅失败 → 可见错误提示', () => {
  it('daemon 拒绝 → code 与 message 原样出现在 settings-error，输入不清空', async () => {
    const rejection: ErrorBody = {
      code: 'invalid_request',
      message: '订阅地址无法拉取：HTTP 404（测试数据，逐字断言）',
    };
    const rig = makeRig({
      capabilities: ['proxy_mode', 'subscriptions', 'subscription_fetch'],
      subscriptions: [],
      addError: rejection,
    });
    renderApp(rig.client);
    await openSettingsPage('proxy_mode、subscriptions、subscription_fetch');

    const input = (await screen.findByTestId('subscription-url-input')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: USER_URL } });
    fireEvent.click(screen.getByTestId('add-subscription-button'));

    // Settings.tsx 真实的失败呈现：订阅卡片内的 <ErrorBox testId="settings-error">（唯一一处）。
    const box = await screen.findByTestId('settings-error');
    expect(box.textContent).toContain(rejection.code);
    expect(box.textContent).toContain(rejection.message);
    // 失败时原文必须留着：清空会让用户重打一遍整个带 token 的 URL。
    expect(input.value).toBe(USER_URL);
  });
});
