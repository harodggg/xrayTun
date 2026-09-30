// @vitest-environment jsdom
//
// 诚实性验收（ux / task-3）—— 独立验收：ui 自己说「通过」不算。
//
// 本文件只做一件事：用一个**测试用 client** 注入契约数据，逐条断言界面上的每个
// 显示项都能追回注入的字段值，且未知不会被显示成 0、失败不会被改写成话术。
//
// 关于「测试数据」的声明（I3 要求）：
//   下面的 fixture 全部用 `src/transport/contract.ts` 的类型构造，并且集中放在本文件
//   顶部。它们是**测试数据**，不是运行时数据源；apps/ui/src 里没有任何 preview/mock。
//
// 关于「测试必须能真的失败」：
//   每条断言都配一个 `mutated(name, 真值, 破坏值)` 开关。正常运行（未设置 XT_TEST_MUTATE）
//   时永远返回真值；设置 `XT_TEST_MUTATE=<name>` 时故意注入与断言矛盾的破坏值，
//   该条断言必须变红。反向验证的实测结果记在 docs/ux/LIVE-EVIDENCE.md。

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
  Stage,
  SubscriptionView,
} from '../../src/transport/contract';

afterEach(cleanup);

// ------------------------------------------------------------------ 反向验证开关

const MUTATION = process.env.XT_TEST_MUTATE ?? '';

const M = {
  statsPresent: '1-stats-present',
  stageDisconnected: '2-stage-disconnected',
  errorDropped: '3-error-dropped',
  bytesWrong: '4-bytes-wrong',
  forbiddenInjected: '5-forbidden-injected',
} as const;

/** 正常运行时返回 real；XT_TEST_MUTATE=name 时返回 broken（用于证明断言不是恒真）。 */
function mutated<T>(name: string, real: T, broken: T): T {
  return MUTATION === name ? broken : real;
}

// ------------------------------------------------------------------ 测试数据（契约类型构造）

const T0 = 1_700_000_000_000;

function hello(capabilities: Capability[]): DaemonHello {
  return {
    daemon_version: '0.0.0-test',
    protocol_version: 1,
    capabilities,
    pid: 4242,
    started_at_ms: T0,
  };
}

const NODE_ONE: NodeView = {
  id: 'node-1',
  name: '测试节点一',
  protocol: 'vless',
  endpoint: '127.0.0.1:443',
  source: { kind: 'subscription', id: 'sub-test' },
};

const NODE_ONE_PROBED: NodeView = {
  ...NODE_ONE,
  probe: { node_id: 'node-1', ttfb_ms: 137, at_ms: T0 },
};

const SETTINGS: SettingsView = {
  socks_listen: '127.0.0.1:1080',
  selected_node: 'node-1',
  log_level: 'info',
};

const SUBSCRIPTIONS: SubscriptionView[] = [
  { id: 'sub-test', url: 'http://127.0.0.1/sub.txt', node_count: 1, fetched_at_ms: T0 },
];

const DISCONNECTED: ConnectionView = {
  stage: 'disconnected',
  datapath: {},
};

interface FakeClientConfig {
  /** null 表示 hello 永不返回（模拟「尚未握手」：能力未知）。 */
  hello?: DaemonHello | null;
  view: ConnectionView;
  nodes?: NodeView[];
  settings?: SettingsView | null;
  subscriptions?: SubscriptionView[];
}

interface FakeClient {
  client: DaemonClient;
  emit: (event: DaemonEvent) => void;
}

function makeClient(cfg: FakeClientConfig): FakeClient {
  const handlers = new Set<(e: DaemonEvent) => void>();
  const client: DaemonClient = {
    hello: () =>
      cfg.hello === null ? new Promise<DaemonHello>(() => {}) : Promise.resolve(cfg.hello ?? hello([])),
    subscribe: () => Promise.resolve(),
    onEvent: (handler) => {
      handlers.add(handler);
      return () => handlers.delete(handler);
    },
    status: () => Promise.resolve(cfg.view),
    connect: () => Promise.resolve(),
    disconnect: () => Promise.resolve(),
    switchNode: () => Promise.resolve(),
    listNodes: () => Promise.resolve(cfg.nodes ?? []),
    probeNodes: () => Promise.resolve(),
    getSettings: () => Promise.resolve(cfg.settings ?? SETTINGS),
    patchSettings: () => Promise.resolve(),
    listSubscriptions: () => Promise.resolve(cfg.subscriptions ?? []),
    addSubscription: () => Promise.resolve(),
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

function renderApp(client: DaemonClient) {
  return render(
    <DaemonProvider client={client}>
      <App />
    </DaemonProvider>,
  );
}

const CAPS_ALL: Capability[] = ['proxy_mode', 'stats', 'probe', 'subscriptions'];

// ------------------------------------------------------------------ 禁语 / 0 字节扫描器

const FORBIDDEN_PHRASES = [
  '重试',
  '自动重试',
  '正在重试',
  '自动恢复',
  '正在恢复连接',
  '自动重连',
  '已为你切换',
  '已切换',
  '自动选择',
  '已自动',
  '已保护',
  '安全保护',
  '已接管',
  '兜底',
  '回落',
  '即将支持',
  '敬请期待',
] as const;

function scanForbidden(text: string): string[] {
  return FORBIDDEN_PHRASES.filter((phrase) => text.includes(phrase));
}

// 负向后行断言：不把 「10 B」「1.0 B」误判成「未知被显示成 0」。
const ZERO_BYTES = /(?<![\d.])0(?:\.0+)?\s*(?:B|B\/s|字节|bytes?|KB|MB|GB)\b/gi;

function scanZeroBytes(text: string): string[] {
  return text.match(ZERO_BYTES) ?? [];
}

// ------------------------------------------------------------------ A1 未采样 ≠ 0

describe('A1 stats 缺失 → 未采样，且不出现 0 字节', () => {
  it('显示「未采样」而不是 0 B', async () => {
    // 反向验证：stats 本应缺失；破坏值给一个真实样本，断言必须随之失败。
    const stats = mutated(M.statsPresent, undefined, {
      uplink_bytes: 4096,
      downlink_bytes: 2048,
      sampled_at_ms: T0,
    });
    const { client } = makeClient({
      hello: hello(['proxy_mode', 'stats']),
      view: { ...DISCONNECTED, stats },
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(client);

    const uplink = await screen.findByTestId('stats-uplink');
    expect(uplink.textContent).toBe('未采样');
    expect(screen.getByTestId('stats-downlink').textContent).toBe('未采样');
    expect(screen.getByTestId('stats-sampled-at').textContent).toBe('未采样');

    // 整页不得把未知说成 0 字节。
    expect(scanZeroBytes(document.body.textContent ?? '')).toEqual([]);
  });

  it('采样成功且真的是 0 时，显示真值 0 而不是「未采样」', async () => {
    const { client } = makeClient({
      hello: hello(['proxy_mode', 'stats']),
      view: {
        ...DISCONNECTED,
        stats: { uplink_bytes: 0, downlink_bytes: 0, sampled_at_ms: T0 },
      },
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(client);

    const uplink = await screen.findByTestId('stats-uplink');
    expect(uplink.textContent).not.toBe('未采样');
    expect(uplink.textContent).toContain('0');
  });
});

// ------------------------------------------------------------------ A2 connecting 阶段

describe('A2 stage=connecting → 连接按钮 disabled 且有阶段文案', () => {
  const PHASES = [
    ['preparing_config', '正在生成配置'],
    ['starting_core', '正在启动核心进程'],
    ['awaiting_ready', '正在等待核心可连'],
    ['committing_routes', '正在提交路由'],
  ] as const;

  for (const [phase, label] of PHASES) {
    it(`phase=${phase} → 按钮 disabled，文案「${label}」，code 原样`, async () => {
      // 反向验证：破坏值让 stage 落在 disconnected（此时按钮可点、阶段文案消失）。
      const stage = mutated<Stage>(M.stageDisconnected, 'connecting', 'disconnected');
      const { client } = makeClient({
        hello: hello(CAPS_ALL),
        view: { stage, phase, node_id: 'node-1', datapath: {} },
        nodes: [NODE_ONE],
        settings: SETTINGS,
      });
      renderApp(client);

      const button = (await screen.findByTestId('connect-button')) as HTMLButtonElement;
      expect(button.disabled).toBe(true);

      const phaseLabel = await screen.findByTestId('phase-label');
      expect(phaseLabel.textContent).toBe(label);
      expect(screen.getByTestId('phase-code').textContent).toBe(phase);
    });
  }
});

// ------------------------------------------------------------------ A3 失败原样显示

describe('A3 last_error → code 与 message 原样显示', () => {
  it('不加工、不翻译、不替换成成功话术', async () => {
    const MESSAGE = '核心进程在就绪前退出：exit status 1（测试数据，逐字断言）';
    // 反向验证：破坏值把它抹掉，断言必须失败。
    const lastError = mutated(M.errorDropped, { code: 'core_exited_early' as const, message: MESSAGE }, undefined);
    const { client } = makeClient({
      hello: hello(CAPS_ALL),
      view: { ...DISCONNECTED, last_error: lastError },
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(client);

    const box = await screen.findByTestId('last-error');
    expect(box.textContent).toContain('core_exited_early');
    expect(box.textContent).toContain(MESSAGE);
    // 徽章仍是 stage 的逐字文案，不被错误块替换。
    expect(screen.getByTestId('stage-badge').textContent).toContain('未连接');
  });
});

// ------------------------------------------------------------------ A4 数值可溯源

describe('A4 每个显示的字节数/延迟都能追溯到注入的契约字段', () => {
  it('改注入的字节数与 TTFB → 界面跟着变', async () => {
    // 期望值是**固定常量**，注入值才受反向验证开关影响 —— 否则断言会跟着被改的注入值一起走，
    // 变成自我一致的恒真断言（第一版就是这样，反向验证时没变红，已修正）。
    const EXPECTED_UP_A = 1234567;
    const expectedDownA = 7654321;
    const upA = mutated(M.bytesWrong, EXPECTED_UP_A, 999999);
    const first = makeClient({
      hello: hello(CAPS_ALL),
      view: { ...DISCONNECTED, stats: { uplink_bytes: upA, downlink_bytes: expectedDownA, sampled_at_ms: T0 } },
      nodes: [NODE_ONE_PROBED],
      settings: SETTINGS,
    });
    renderApp(first.client);
    const uplinkA = await screen.findByTestId('stats-uplink');
    expect(uplinkA.textContent).toContain(String(EXPECTED_UP_A));
    expect(screen.getByTestId('stats-downlink').textContent).toContain(String(expectedDownA));

    // TTFB 来自 NodeView.probe.ttfb_ms（137），必须逐字出现在节点页。
    fireEvent.click(screen.getByTestId('nav-nodes'));
    const probeA = await screen.findByTestId('node-probe');
    expect(probeA.textContent).toContain('137');
    expect(probeA.textContent).toContain('ms');

    cleanup();

    // 换一组注入值：界面必须跟着变，证明上面不是写死的常量。
    const upB = 240000;
    const downB = 12345;
    const second = makeClient({
      hello: hello(CAPS_ALL),
      view: { ...DISCONNECTED, stats: { uplink_bytes: upB, downlink_bytes: downB, sampled_at_ms: T0 } },
      nodes: [{ ...NODE_ONE_PROBED, probe: { node_id: 'node-1', ttfb_ms: 42, at_ms: T0 } }],
      settings: SETTINGS,
    });
    renderApp(second.client);
    const uplinkB = await screen.findByTestId('stats-uplink');
    expect(uplinkB.textContent).toContain(String(upB));
    expect(uplinkB.textContent).not.toContain(String(EXPECTED_UP_A));
    expect(screen.getByTestId('stats-downlink').textContent).toContain(String(downB));

    fireEvent.click(screen.getByTestId('nav-nodes'));
    const probeB = await screen.findByTestId('node-probe');
    expect(probeB.textContent).toContain('42');
    expect(probeB.textContent).not.toContain('137');
  });
});

// ------------------------------------------------------------------ A5 禁语扫描

describe('A5 界面不存在没有字段证据的话术', () => {
  it('扫描器本身能报错（证明断言不是恒真）', () => {
    expect(scanForbidden('本来一切正常')).toEqual([]);
    for (const phrase of FORBIDDEN_PHRASES) {
      expect(scanForbidden(`前缀${phrase}后缀`)).toContain(phrase);
    }
  });

  it('渲染出的界面文本无禁语', async () => {
    const { client } = makeClient({
      hello: hello(CAPS_ALL),
      view: {
        ...DISCONNECTED,
        last_error: { code: 'io', message: '连接被拒绝' },
        stats: { uplink_bytes: 1024, downlink_bytes: 2048, sampled_at_ms: T0 },
      },
      nodes: [NODE_ONE],
      settings: SETTINGS,
      subscriptions: SUBSCRIPTIONS,
    });
    renderApp(client);
    await screen.findByTestId('stage-badge');
    // 遍历全部页面，避免只扫了首屏。
    for (const page of ['nav-nodes', 'nav-logs', 'nav-settings', 'nav-dashboard']) {
      fireEvent.click(screen.getByTestId(page));
      await waitFor(() => expect(screen.getByTestId(page)).toBeTruthy());
    }
    // 反向验证：破坏值把禁语注入扫描文本，断言必须失败。
    const scanned = mutated(
      M.forbiddenInjected,
      document.body.textContent ?? '',
      `${document.body.textContent ?? ''}\n已为你切换到最快的节点`,
    );
    expect(scanForbidden(scanned)).toEqual([]);
  });
});

// ------------------------------------------------------------------ 能力宣告 = 唯一事实源

describe('规则 B 能力宣告 = 唯一事实源（DaemonHello.capabilities）', () => {
  it('capabilities 为空 → 连接以外的能力入口一个都不出现', async () => {
    const { client } = makeClient({
      hello: hello([]),
      view: { ...DISCONNECTED, stats: { uplink_bytes: 4096, downlink_bytes: 4096, sampled_at_ms: T0 } },
      nodes: [NODE_ONE],
      settings: SETTINGS,
      subscriptions: SUBSCRIPTIONS,
    });
    renderApp(client);
    await screen.findByTestId('stage-badge');

    // 设置页：等 hello 真的落地（能力字段被渲染出来）再断言，避免「还没加载」被当成「没有」。
    fireEvent.click(screen.getByTestId('nav-settings'));
    await waitFor(() =>
      expect(screen.getByTestId('daemon-capabilities').textContent).toBe('未宣告任何能力'),
    );
    expect(screen.queryByTestId('subscriptions-section')).toBeNull();
    expect(screen.queryByTestId('add-subscription-button')).toBeNull();
    expect(screen.queryByTestId('subscription-row')).toBeNull();

    // 节点页：没有 probe 能力 → 没有探测入口。
    fireEvent.click(screen.getByTestId('nav-nodes'));
    expect(screen.queryByTestId('probe-button')).toBeNull();
    // 节点目录数据本身是数据，不是「入口」，仍然显示。
    expect(screen.getByTestId('node-name').textContent).toBe('测试节点一');

    // 仪表盘：没有 stats 能力 → 整个统计卡不渲染（而不是渲染一张「未采样」的假卡）。
    fireEvent.click(screen.getByTestId('nav-dashboard'));
    expect(screen.queryByTestId('stats-uplink')).toBeNull();
  });

  it('capabilities 含 probe → 探测入口出现（正向对照）', async () => {
    const { client } = makeClient({
      hello: hello(['proxy_mode', 'probe']),
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(client);
    fireEvent.click(await screen.findByTestId('nav-nodes'));
    const probeButton = await screen.findByTestId('probe-button');
    expect(probeButton.textContent).toBe('探测');
  });

  it('subscriptions（无 subscription_fetch）→ 只读订阅区出现，但没有任何拉取入口', async () => {
    // 这正是本轮 daemon 的真实形态：能解析本地订阅文件，但不能远端拉取。
    // 只读区必须有；「添加 / 刷新」入口一个都不能有（否则就是按不动的假控件）。
    const { client } = makeClient({
      hello: hello(['proxy_mode', 'subscriptions']),
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
      subscriptions: SUBSCRIPTIONS,
    });
    renderApp(client);
    fireEvent.click(await screen.findByTestId('nav-settings'));

    expect(await screen.findByTestId('subscriptions-section')).toBeTruthy();
    expect(screen.getByTestId('subscription-row')).toBeTruthy();
    expect(screen.queryByTestId('add-subscription-button')).toBeNull();
    expect(screen.queryByTestId('refresh-subscription-button')).toBeNull();
    expect(screen.queryByTestId('subscription-url-input')).toBeNull();
  });

  it('subscriptions + subscription_fetch → 拉取入口才出现（正向对照）', async () => {
    const { client } = makeClient({
      hello: hello(['proxy_mode', 'subscriptions', 'subscription_fetch']),
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
      subscriptions: SUBSCRIPTIONS,
    });
    renderApp(client);
    fireEvent.click(await screen.findByTestId('nav-settings'));
    expect(await screen.findByTestId('subscriptions-section')).toBeTruthy();
    expect(screen.getByTestId('add-subscription-button')).toBeTruthy();
    expect(screen.getByTestId('subscription-url-input')).toBeTruthy();
  });

  it('TUN 开关同样由 tun_mode 门控（不含 → 无，含 → 出现）', async () => {
    // 不含：本轮 daemon 的真实情况 —— 页面上找不到任何 TUN 控件或文案。
    const withoutTun = makeClient({
      hello: hello(['proxy_mode', 'stats', 'probe', 'subscriptions', 'subscription_fetch']),
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(withoutTun.client);
    await screen.findByTestId('stage-badge');
    expect(screen.queryByTestId('run-mode-select')).toBeNull();
    expect(document.body.textContent ?? '').not.toContain('TUN');
    cleanup();

    // 含：能力宣告 = 唯一事实源 —— 入口必须出现（否则无法区分门控与「功能根本没写」）。
    const withTun = makeClient({
      hello: hello(['proxy_mode', 'stats', 'tun_mode']),
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(withTun.client);
    expect(await screen.findByTestId('run-mode-select')).toBeTruthy();
  });

  it('hello 尚未到达（能力未知）→ 不默认宣告任何能力', async () => {
    const { client } = makeClient({
      hello: null, // Provider 会一直等 hello
      view: DISCONNECTED,
      nodes: [NODE_ONE],
      settings: SETTINGS,
    });
    renderApp(client);
    // 连状态快照都还没有时，徽章只能是「未知」。
    expect((await screen.findByTestId('stage-badge')).textContent).toBe('未知');

    fireEvent.click(screen.getByTestId('nav-settings'));
    expect(screen.getByTestId('daemon-version').textContent).toBe('未知');
    expect(screen.queryByTestId('subscriptions-section')).toBeNull();

    fireEvent.click(screen.getByTestId('nav-nodes'));
    expect(screen.queryByTestId('probe-button')).toBeNull();

    fireEvent.click(screen.getByTestId('nav-dashboard'));
    expect(screen.queryByTestId('stats-uplink')).toBeNull();
  });
});

// ------------------------------------------------------------------ 导航禁令 + 空态

describe('IA 导航禁令与空态', () => {
  it('导航恰好 4 项，没有未实现能力的入口', async () => {
    const { client } = makeClient({ hello: hello(CAPS_ALL), view: DISCONNECTED });
    renderApp(client);
    await screen.findByTestId('nav-dashboard');

    const navTestIds = Array.from(document.querySelectorAll('[data-testid^="nav-"]')).map(
      (el) => el.getAttribute('data-testid'),
    );
    expect(navTestIds.sort()).toEqual(['nav-dashboard', 'nav-logs', 'nav-nodes', 'nav-settings']);

    const bodyText = document.body.textContent ?? '';
    for (const forbiddenEntry of ['TUN', '特权助手', '拓扑', '地球', '定位']) {
      expect(bodyText).not.toContain(forbiddenEntry);
    }
  });

  it('空态：无节点 / 无日志 给出逐字空态文案，不填充假数据', async () => {
    const { client } = makeClient({
      hello: hello(CAPS_ALL),
      view: DISCONNECTED,
      nodes: [],
    });
    renderApp(client);

    fireEvent.click(await screen.findByTestId('nav-nodes'));
    expect(await screen.findByTestId('nodes-empty')).toBeTruthy();
    expect(screen.getByText('暂无节点')).toBeTruthy();

    fireEvent.click(screen.getByTestId('nav-logs'));
    expect(await screen.findByTestId('logs-empty')).toBeTruthy();
    expect(screen.getByText('暂无日志')).toBeTruthy();
  });
});
