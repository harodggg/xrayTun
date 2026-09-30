// xraytun-next · Tauri 传输（生产形状）—— **未经 macOS 真机验证**
//
// 诚实声明（不是免责套话，来源可查：本轮没有 macOS 构建、没有真机、没有任何一次真实的
// invoke/listen 往返）：
//  * 这个文件**不构成「可用」声明**。它只是按契约形状写出来的生产实现草案，
//    未经 macOS 真机验证，所以不要在任何界面文案或报告里把它说成「已可用」。
//  * 它依赖的桥在 Rust 侧**尚不存在**：命令名 `xt_daemon_request`、事件名 `xt_daemon_event`、
//    参数形状 `{ socketPath, id, request }` 都是本轮约定的形状。桥对不上时 invoke 会 reject，
//    错误会如实冒到界面上（不会静默吞掉、也不会换一条路）。
//  * 本轮的活体验收走的是 unixSocket.ts（真 AF_UNIX）；这条通道只由打包后的应用使用。
//
// 为什么把所有请求都塞进一个命令：请求 id、pending 表、seq 跳号检测的逻辑与 unixSocket.ts
// 保持一致 —— 两份传输对同一份契约给出同一套语义，界面才不会因为跑在哪条通道上而行为不同。

import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { errorBody, pickResponse, pickSettledResponse, requireArrayField, toErrorBody } from './client';
import type { DaemonClient } from './client';
import { CLIENT_VERSION, PROTOCOL_VERSION } from './contract';
import type {
  ConnectionView,
  DaemonEvent,
  DaemonHello,
  ErrorBody,
  LogLine,
  NodeView,
  Outcome,
  Request,
  Response,
  RunMode,
  SettingsPatch,
  SettingsView,
  SubscriptionView,
  Topic,
} from './contract';

export interface TauriClientOptions {
  /** 覆盖桥的命令名（默认 `xt_daemon_request`）。 */
  commandName?: string;
  /** 覆盖桥的事件名（默认 `xt_daemon_event`）。 */
  eventName?: string;
  /** hello 里如实上报的客户端版本。 */
  clientVersion?: string;
}

interface PendingRequest {
  resolve: (response: Response) => void;
  reject: (error: ErrorBody) => void;
}

export function createTauriClient(socketPath: string, options: TauriClientOptions = {}): DaemonClient {
  const commandName = options.commandName ?? 'xt_daemon_request';
  const eventName = options.eventName ?? 'xt_daemon_event';
  const clientVersion = options.clientVersion ?? CLIENT_VERSION;

  const pending = new Map<number, PendingRequest>();
  const eventHandlers = new Set<(event: DaemonEvent) => void>();
  const errorHandlers = new Set<(error: ErrorBody) => void>();
  let unlisten: UnlistenFn | null = null;
  let listenerReady: Promise<void> | null = null;
  let nextRequestId = 1;
  let expectedSeq = 1;
  let closed = false;
  let fatalError: ErrorBody | null = null;

  function notifyTransportError(error: ErrorBody): void {
    for (const handler of [...errorHandlers]) handler(error);
  }

  function fail(error: ErrorBody): void {
    if (fatalError === null) fatalError = error;
    closed = true;
    const inFlight = [...pending.values()];
    pending.clear();
    for (const entry of inFlight) entry.reject(error);
    notifyTransportError(error);
  }

  function handleEvent(payload: unknown): void {
    if (typeof payload !== 'object' || payload === null) {
      fail(errorBody('internal', 'Tauri 事件不是对象', { received: String(payload) }));
      return;
    }
    const frame = payload as { seq?: unknown; event?: unknown };
    if (typeof frame.seq !== 'number' || typeof frame.event !== 'object' || frame.event === null) {
      fail(errorBody('internal', 'Tauri 事件缺少 seq/event 字段', { received: payload }));
      return;
    }
    if (frame.seq !== expectedSeq) {
      // 与 unixSocket.ts 同一条规则：跳号就是有帧丢了，静默继续会让界面显示假的完整序列。
      fail(
        errorBody('internal', `事件 seq 跳号：期望 ${expectedSeq}，收到 ${frame.seq}`, {
          expected: expectedSeq,
          received: frame.seq,
        }),
      );
      return;
    }
    expectedSeq += 1;
    for (const handler of [...eventHandlers]) handler(frame.event as DaemonEvent);
  }

  function ensureListener(): Promise<void> {
    if (fatalError !== null) return Promise.reject(fatalError);
    if (closed) return Promise.reject(errorBody('io', '客户端已关闭，不再接受新请求'));
    if (listenerReady !== null) return listenerReady;

    listenerReady = listen<unknown>(eventName, (event) => {
      handleEvent(event.payload);
    })
      .then((off) => {
        // 关闭发生在监听注册完成之前：注册完立刻取消，不留悬挂的监听。
        if (closed) {
          off();
          return;
        }
        unlisten = off;
      })
      .catch((cause: unknown) => {
        const body = toErrorBody(cause);
        fail(body);
        throw body;
      });
    return listenerReady;
  }

  function request(message: Request): Promise<Response> {
    const id = nextRequestId;
    nextRequestId += 1;
    return ensureListener().then(
      () =>
        new Promise<Response>((resolve, reject) => {
          if (fatalError !== null) {
            reject(fatalError);
            return;
          }
          pending.set(id, { resolve, reject });
          void invoke<Outcome>(commandName, { socketPath, id, request: message }).then(
            (outcome) => {
              const entry = pending.get(id);
              // 已经被致命失败清空：错误已经报过一次，这里不再重复报。
              if (entry === undefined) return;
              if (typeof outcome !== 'object' || outcome === null || (outcome.status !== 'ok' && outcome.status !== 'error')) {
                pending.delete(id);
                entry.reject(errorBody('internal', `${commandName} 的返回值不是 Outcome`, { received: outcome }));
                return;
              }
              pending.delete(id);
              if (outcome.status === 'ok') {
                entry.resolve(outcome.response);
              } else {
                entry.reject(outcome.error);
              }
            },
            (cause: unknown) => {
              const entry = pending.get(id);
              if (entry === undefined) return;
              pending.delete(id);
              entry.reject(toErrorBody(cause));
            },
          );
        }),
    );
  }

  return {
    async hello(): Promise<DaemonHello> {
      const response = await request({ op: 'hello', client_version: clientVersion, protocol_version: PROTOCOL_VERSION });
      return pickResponse(response, 'hello');
    },

    async subscribe(topics: Topic[]): Promise<void> {
      const response = await request({ op: 'subscribe', topics });
      const granted = pickResponse(response, 'subscribed');
      requireArrayField(granted.topics, 'topics');
      const missing = topics.filter((topic) => !granted.topics.includes(topic));
      if (missing.length > 0) {
        throw errorBody('internal', `daemon 未受理这些主题：${missing.join(', ')}`, {
          requested: topics,
          granted: granted.topics,
        });
      }
    },

    onEvent(handler: (event: DaemonEvent) => void): () => void {
      eventHandlers.add(handler);
      return () => {
        eventHandlers.delete(handler);
      };
    },

    onTransportError(handler: (error: ErrorBody) => void): () => void {
      errorHandlers.add(handler);
      return () => {
        errorHandlers.delete(handler);
      };
    },

    async status(): Promise<ConnectionView> {
      return pickResponse(await request({ op: 'status' }), 'status');
    },

    async connect(nodeId: string, mode: RunMode): Promise<void> {
      pickSettledResponse(await request({ op: 'connect', node_id: nodeId, mode }));
    },

    async disconnect(): Promise<void> {
      pickSettledResponse(await request({ op: 'disconnect' }));
    },

    async switchNode(nodeId: string): Promise<void> {
      pickSettledResponse(await request({ op: 'switch_node', node_id: nodeId }));
    },

    async listNodes(): Promise<NodeView[]> {
      const response = pickResponse(await request({ op: 'list_nodes' }), 'nodes');
      return requireArrayField(response.nodes, 'nodes') as NodeView[];
    },

    async probeNodes(ids: string[]): Promise<void> {
      pickSettledResponse(await request({ op: 'probe_nodes', node_ids: ids }));
    },

    async getSettings(): Promise<SettingsView> {
      return pickResponse(await request({ op: 'get_settings' }), 'settings');
    },

    async patchSettings(patch: SettingsPatch): Promise<void> {
      pickSettledResponse(await request({ op: 'patch_settings', patch }));
    },

    async listSubscriptions(): Promise<SubscriptionView[]> {
      const response = pickResponse(await request({ op: 'list_subscriptions' }), 'subscriptions');
      return requireArrayField(response.subscriptions, 'subscriptions') as SubscriptionView[];
    },

    async addSubscription(url: string): Promise<void> {
      pickSettledResponse(await request({ op: 'add_subscription', url }));
    },

    async refreshSubscription(id: string): Promise<void> {
      pickSettledResponse(await request({ op: 'refresh_subscription', id }));
    },

    async tailLogs(lines: number): Promise<LogLine[]> {
      const response = pickResponse(await request({ op: 'tail_logs', lines }), 'logs');
      return requireArrayField(response.logs, 'logs') as LogLine[];
    },

    close(): void {
      if (closed) return;
      closed = true;
      const body = errorBody('io', '客户端已关闭，未完成的请求全部失败');
      const inFlight = [...pending.values()];
      pending.clear();
      for (const entry of inFlight) entry.reject(body);
      // 取消监听是异步的，而 close(): void 不能等；把 unlisten 排到已就绪的 promise 上。
      const off = unlisten;
      unlisten = null;
      if (off !== null) off();
    },
  };
}
