// xraytun-next · 事件驱动的 daemon 状态源
//
// 为什么是 Provider + useDaemon 而不是每个组件自己 subscribe：一台连接只允许存在一份
// 「当前事实」（谁也不需要去猜另一个组件的请求结果）。Provider 负责把 onEvent 的每一帧
// 落到 state 上，界面只读 state。
//
// 这个文件里**没有定时器、没有轮询、没有自动重连**（I1/I2）：
//  * 挂载时做一次 hello → subscribe → status，再各拉一次初始快照；
//  * 之后 state 只在事件到达时改变；
//  * 传输断了就停在「断了」，transportError 如实记录，等用户按「重新连接」这个显式动作
//    （reconnect：用**同一个** client 重跑上面那条引导链）。界面里没有任何东西会自己再连一次。

import { createContext, createElement, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import type { JSX, ReactNode } from 'react';
import type { DaemonClient } from '../transport/client';
import { errorBody, toErrorBody } from '../transport/client';
import { ALL_TOPICS } from '../transport/contract';
import type {
  ConnectionView,
  DaemonHello,
  ErrorBody,
  LogLine,
  NodeView,
  Notice,
  SettingsView,
  SubscriptionView,
} from '../transport/contract';

/**
 * 日志缓冲上限。为什么要有上限：日志事件速率由 daemon 决定，界面抱着无限增长的数组
 * 迟早把整个 WebView 拖死；截掉的是最老的**真实**日志，不伪造任何一条。
 * 需要完整历史时用 client.tailLogs（真实来源只有日志本身）。
 */
export const LOG_BUFFER_LIMIT = 1000;

/** 同上的理由，notice 更少更值钱（失败/降级提示），留最近 100 条。 */
export const NOTICE_BUFFER_LIMIT = 100;

export interface DaemonState {
  /** 还没有任何快照时是 null —— 「不知道」不能显示成「未连接」。 */
  connection: ConnectionView | null;
  nodes: NodeView[];
  logs: LogLine[];
  subscriptions: SubscriptionView[];
  settings: SettingsView | null;
  hello: DaemonHello | null;
  /** 最近一次**传输层**致命失败；只由传输层设置，不因为后来某次请求成功就被悄悄擦掉。 */
  transportError: ErrorBody | null;
  /**
   * 追加字段（冻结形状里没有它）：Event::Notice 是真实事件，而状态里没有落脚处，
   * 静默丢弃就是丢事件（I3）。已经报给 lead 备案。
   */
  notices: Notice[];
}

const INITIAL_STATE: DaemonState = {
  connection: null,
  nodes: [],
  logs: [],
  subscriptions: [],
  settings: null,
  hello: null,
  transportError: null,
  notices: [],
};

type RefreshKind = 'settings' | 'subscriptions';

type DaemonContextValue = DaemonState & {
  client: DaemonClient;
  /**
   * 用户**显式**发起的一次重新引导（清错误 → hello → subscribe → status → 三个快照）。
   * 它存在的理由：这条引导链任何一步失败，界面就停在「断了」—— 重跑是唯一的恢复路径，
   * 而它必须由用户按下，不能由界面自己发起（I1/I2）。
   */
  reconnect: () => void;
  /** reconnect 正在跑。按钮据此变灰，避免连点重复发起。 */
  reconnectPending: boolean;
};

const DaemonContext = createContext<DaemonContextValue | null>(null);

// children 用可选：React 的惯例（PropsWithChildren）如此，也因为 createElement(Provider, {client}, child)
// 这种把 children 放在第三参数的写法在 children 必填时会直接报 TS 错。
export function DaemonProvider(props: { client: DaemonClient; children?: ReactNode }): JSX.Element {
  const { client, children } = props;
  const [state, setState] = useState<DaemonState>(INITIAL_STATE);
  const [reconnectPending, setReconnectPending] = useState(false);

  // 为什么用 ref 而不是 effect 里的局部变量：刷新包装器（onMutation 之后重新拉快照）
  // 在 effect 之外，也需要知道组件还在不在。
  const mountedRef = useRef(false);
  // 为什么连点保护要用 ref、而不是只信上面的 state：setState 要等下一次渲染才可见，
  // 两次点击有可能在同一帧里都读到「没有进行中」；ref 是同步事实源，state 只负责让按钮变灰。
  const reconnectInFlightRef = useRef(false);
  // status 快照与 state 事件的到达顺序没有保证；用它判断快照是否已经落后于事件。
  const stateEventsSeenRef = useRef(0);

  const update = useCallback((next: (prev: DaemonState) => DaemonState) => {
    if (mountedRef.current) setState(next);
  }, []);

  const apply = useCallback(
    (patch: Partial<DaemonState>) => {
      update((prev) => ({ ...prev, ...patch }));
    },
    [update],
  );

  const refresh = useCallback(
    async (kind: RefreshKind) => {
      try {
        // 变更类请求的应答（Ok/Accepted）里没有新值，所以变更成功后拉一次受影响的快照。
        // 这不是轮询：每次用户动作恰好一次请求，没有定时器，也不会失败后自己重来。
        const [nodes, subscriptions, settings] = await Promise.all([
          kind === 'subscriptions' ? client.listNodes() : null,
          kind === 'subscriptions' ? client.listSubscriptions() : null,
          kind === 'settings' ? client.getSettings() : null,
        ]);
        apply({
          ...(nodes === null ? {} : { nodes }),
          ...(subscriptions === null ? {} : { subscriptions }),
          ...(settings === null ? {} : { settings }),
        });
      } catch (error) {
        apply({ transportError: toErrorBody(error) });
      }
    },
    [apply, client],
  );

  /**
   * 完整引导链：hello → subscribe → status → 三个并行快照。
   *
   * 为什么提成具名函数：挂载时与用户按「重新连接」时必须是**同一条**路径。两处各维护一份
   * 迟早会漂移，而「重新连接之后的界面和刚启动时不一样」正是这类漂移最难查的症状。
   *
   * 失败不往外抛，就在这里收敛成 transportError：调用方（挂载 effect、reconnect）无论从哪来
   * 都不会漏掉 catch，也就不会出现没人处理的 rejection。
   */
  const runBootstrap = useCallback(async () => {
    // 先清掉上一轮的传输错误：错误盒在重新连接期间必须消失，否则用户没法判断这一按有没有生效。
    // 这不是「成功后悄悄擦掉」—— 清完立刻如实重跑，真失败会再写回一个真实的 ErrorBody。
    apply({ transportError: null });

    try {
      const hello = await client.hello();
      apply({ hello });

      await client.subscribe([...ALL_TOPICS]);

      const seenBeforeStatus = stateEventsSeenRef.current;
      const connection = await client.status();
      // 为什么比较计数：status 的应答可能在一个更新的 state 事件之后才到，
      // 把落后的快照写回去就是显示假事实 —— 过期的快照直接丢掉。
      if (stateEventsSeenRef.current === seenBeforeStatus) {
        apply({ connection });
      }

      const [nodes, subscriptions, settings] = await Promise.all([
        client.listNodes(),
        client.listSubscriptions(),
        client.getSettings(),
      ]);
      apply({ nodes, subscriptions, settings });
    } catch (error) {
      apply({ transportError: toErrorBody(error) });
    }
  }, [apply, client]);

  /**
   * 用户显式发起的重新连接。它是界面上唯一一条恢复路径，没有任何东西会替用户按它。
   *
   * 为什么复用同一个 client 实例（不新建）：打包路径下每条请求都由 Rust 桥转发，而桥在
   * 错误码是 `io` 时会**丢掉缓存的连接**，下一次请求自己重新握手 —— 所以对「冷启动时
   * daemon 还没起来」「daemon 中途挂了」这两类最常见的失败，同一个 client 重跑整条引导链
   * 就是真的恢复。
   *
   * 如实说明它治不了的那一类：客户端自己已经进了**致命态**（`tauri.ts` 的 `fail()` 会把
   * `closed` 与 `fatalError` 永久钉住，之后每个 request 都立刻 reject，包括新建实例也没用
   * 到点上）。那种情况这一按救不回来，错误会原样再显示一次 —— 这里不假装它治得了，也不为此
   * 加任何自动动作。页面上真正会制造致命态的那条路径（WebView 重载后事件 seq 对不上）已经在
   * 壳侧修掉了：见 `apps/desktop/src/bridge.rs` 的会话闸门与 `lib.rs` 的 `on_page_load`。
   *
   * 另一条通道 unixSocket.ts（活体验收 / 无头调试用，不是打包路径）如果 socket 已经死了，
   * 同一个 client 重跑会以同样的 io 错误失败 —— 同样不假装它能恢复。
   */
  const reconnect = useCallback(async () => {
    if (reconnectInFlightRef.current) return;
    reconnectInFlightRef.current = true;
    setReconnectPending(true);
    try {
      await runBootstrap();
    } finally {
      reconnectInFlightRef.current = false;
      // 直接调 setState 而不是走 apply：apply 受 mountedRef 门控（卸载后不写 state 是对的），
      // 但「进行中」这个本地标志无论如何都要复位，否则卸载重挂之后再也没法重新连接。
      if (mountedRef.current) setReconnectPending(false);
    }
  }, [runBootstrap]);

  useEffect(() => {
    mountedRef.current = true;

    // 先挂监听再发请求：否则「请求已受理、事件已到达」的那一帧会被漏掉。
    const offEvent = client.onEvent((event) => {
      switch (event.event) {
        case 'state':
          stateEventsSeenRef.current += 1;
          apply({ connection: event.view });
          return;
        case 'log':
          update((prev) => {
            const logs = prev.logs.length >= LOG_BUFFER_LIMIT
              ? [...prev.logs.slice(prev.logs.length - LOG_BUFFER_LIMIT + 1), event.line]
              : [...prev.logs, event.line];
            return { ...prev, logs };
          });
          return;
        case 'probe':
          update((prev) => {
            const index = prev.nodes.findIndex((node) => node.id === event.result.node_id);
            if (index < 0) {
              // 真实事件指向列表里没有的节点 = 列表比事件旧。如实记录，不静默丢、也不编节点。
              return {
                ...prev,
                transportError: errorBody('internal', `收到未知节点的探测结果：${event.result.node_id}`, {
                  node_id: event.result.node_id,
                }),
              };
            }
            const nodes = prev.nodes.slice();
            nodes[index] = { ...nodes[index], probe: event.result };
            return { ...prev, nodes };
          });
          return;
        case 'notice':
          update((prev) => {
            const notices = prev.notices.length >= NOTICE_BUFFER_LIMIT
              ? [...prev.notices.slice(prev.notices.length - NOTICE_BUFFER_LIMIT + 1), event.notice]
              : [...prev.notices, event.notice];
            return { ...prev, notices };
          });
          return;
      }
    });

    const offTransportError = client.onTransportError?.((error) => {
      apply({ transportError: error });
    });

    // 挂载时的引导链与「重新连接」走同一个函数：同一条路径，两处不会漂移。
    void runBootstrap();

    return () => {
      mountedRef.current = false;
      offEvent();
      offTransportError?.();
    };
  }, [apply, client, runBootstrap, update]);

  /**
   * 为什么把 client 包一层再交给界面：patch_settings / add_subscription /
   * refresh_subscription 的应答里没有新值（契约里是 Ok/Accepted），界面拿到成功之后
   * state 会停在旧值 —— 那就是界面在说假话（I3）。这三个方法成功后各拉一次快照。
   * 其余方法与传输实现逐一同名同形地转发，没有第二条路径。
   */
  const clientForUi = useMemo<DaemonClient>(
    () => ({
      hello: () => client.hello(),
      subscribe: (topics) => client.subscribe(topics),
      onEvent: (handler) => client.onEvent(handler),
      onTransportError: client.onTransportError ? (handler) => client.onTransportError!(handler) : undefined,
      status: () => client.status(),
      connect: (nodeId, mode) => client.connect(nodeId, mode),
      disconnect: () => client.disconnect(),
      switchNode: (nodeId) => client.switchNode(nodeId),
      listNodes: () => client.listNodes(),
      probeNodes: (ids) => client.probeNodes(ids),
      getSettings: () => client.getSettings(),
      patchSettings: async (patch) => {
        await client.patchSettings(patch);
        await refresh('settings');
      },
      listSubscriptions: () => client.listSubscriptions(),
      addSubscription: async (url) => {
        await client.addSubscription(url);
        await refresh('subscriptions');
      },
      refreshSubscription: async (id) => {
        await client.refreshSubscription(id);
        await refresh('subscriptions');
      },
      tailLogs: (lines) => client.tailLogs(lines),
      close: () => client.close(),
    }),
    [client, refresh],
  );

  const value = useMemo<DaemonContextValue>(
    () => ({ ...state, client: clientForUi, reconnect, reconnectPending }),
    [state, clientForUi, reconnect, reconnectPending],
  );

  // 这个文件是 .ts（冻结的文件名），所以不能用 JSX 语法，只有这一处需要 createElement。
  return createElement(DaemonContext.Provider, { value }, children);
}

export function useDaemon(): DaemonContextValue {
  const value = useContext(DaemonContext);
  if (value === null) {
    throw new Error('useDaemon 必须在 <DaemonProvider> 内使用');
  }
  return value;
}
