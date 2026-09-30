// xraytun-next · 事件驱动的 daemon 状态源
//
// 为什么是 Provider + useDaemon 而不是每个组件自己 subscribe：一台连接只允许存在一份
// 「当前事实」（谁也不需要去猜另一个组件的请求结果）。Provider 负责把 onEvent 的每一帧
// 落到 state 上，界面只读 state。
//
// 这个文件里**没有定时器、没有轮询、没有自动重连**（I1/I2）：
//  * 挂载时做一次 hello → subscribe → status，再各拉一次初始快照；
//  * 之后 state 只在事件到达时改变；
//  * 传输断了就停在「断了」，transportError 如实记录，等用户显式动作（重新挂载新 client）。

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

type DaemonContextValue = DaemonState & { client: DaemonClient };

const DaemonContext = createContext<DaemonContextValue | null>(null);

// children 用可选：React 的惯例（PropsWithChildren）如此，也因为 createElement(Provider, {client}, child)
// 这种把 children 放在第三参数的写法在 children 必填时会直接报 TS 错。
export function DaemonProvider(props: { client: DaemonClient; children?: ReactNode }): JSX.Element {
  const { client, children } = props;
  const [state, setState] = useState<DaemonState>(INITIAL_STATE);

  // 为什么用 ref 而不是 effect 里的局部变量：刷新包装器（onMutation 之后重新拉快照）
  // 在 effect 之外，也需要知道组件还在不在。
  const mountedRef = useRef(false);
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

    void (async () => {
      try {
        const hello = await client.hello();
        // hello 成功 = 这条传输确实通了，清掉上一轮挂载残留的传输错误。
        apply({ hello, transportError: null });

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
    })();

    return () => {
      mountedRef.current = false;
      offEvent();
      offTransportError?.();
    };
  }, [apply, client, update]);

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

  const value = useMemo<DaemonContextValue>(() => ({ ...state, client: clientForUi }), [state, clientForUi]);

  // 这个文件是 .ts（冻结的文件名），所以不能用 JSX 语法，只有这一处需要 createElement。
  return createElement(DaemonContext.Provider, { value }, children);
}

export function useDaemon(): DaemonState & { client: DaemonClient } {
  const value = useContext(DaemonContext);
  if (value === null) {
    throw new Error('useDaemon 必须在 <DaemonProvider> 内使用');
  }
  return value;
}
