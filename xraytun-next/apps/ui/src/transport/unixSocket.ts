// xraytun-next · 真 AF_UNIX 传输（node net）
//
// 为什么它存在、而不是只有 Tauri：本轮的**活体验收**（真 daemon、真帧、真字节）就跑在这条
// 通道上 —— 无头环境下不需要 WebView、不需要 macOS 权限，就能验证 UI 数据层。
// 帧格式与 crates/xt-ipc 一致：4 字节大端长度前缀 + 一帧 UTF-8 JSON，判别式 `kind`。
//
// 三条如实暴露（I2/I3）：
//  * 连接失败 / socket 断开 → 报 `io` 错误，**不重连、不重试**（重连是回落，需要用户显式动作）。
//  * 事件 seq 跳号 → 报 `internal`：连接已不可信，继续用下去只会产生假事实。
//  * 应答形状不对 → 报 `internal` 并带上收到的内容，不猜、不吞。
//
// 限制（如实写在文件头）：这条通道需要 Node 的 AF_UNIX。**非 Tauri 的浏览器/WebView 环境里
// 它不可用** —— 那种环境不会假装连上，而是以 `transportError`（code `io`）失败。

import type { Socket } from 'node:net';
import { errorBody, pickResponse, pickSettledResponse, requireArrayField } from './client';
import type { DaemonClient } from './client';
import { CLIENT_VERSION, MAX_FRAME_BYTES, PROTOCOL_VERSION } from './contract';
import type {
  ConnectionView,
  DaemonEvent,
  DaemonHello,
  ErrorBody,
  Frame,
  LogLine,
  NodeView,
  Request,
  Response,
  RunMode,
  SettingsPatch,
  SettingsView,
  SubscriptionView,
  Topic,
} from './contract';

export interface UnixSocketClientOptions {
  /** hello 里如实上报的客户端版本，默认取 contract.ts 的常量。 */
  clientVersion?: string;
  /** 覆盖单帧上限（默认 1 MiB，与 xt-contract 的 MAX_FRAME_BYTES 一致）。 */
  maxFrameBytes?: number;
}

interface PendingRequest {
  resolve: (response: Response) => void;
  reject: (error: ErrorBody) => void;
}

/**
 * 惰性建连的 AF_UNIX 客户端：第一次发请求时才 connect。
 * 为什么惰性：工厂函数是同步的（界面在渲染路径上就要拿到 client），而 net.connect 是异步的；
 * 把建连藏进第一次请求里，接口就不需要多一个 `open()` 或一个会失败的构造阶段。
 */
export function createUnixSocketClient(socketPath: string, options: UnixSocketClientOptions = {}): DaemonClient {
  const clientVersion = options.clientVersion ?? CLIENT_VERSION;
  const maxFrameBytes = options.maxFrameBytes ?? MAX_FRAME_BYTES;

  const pending = new Map<number, PendingRequest>();
  const eventHandlers = new Set<(event: DaemonEvent) => void>();
  const errorHandlers = new Set<(error: ErrorBody) => void>();
  let socket: Socket | null = null;
  let connecting: Promise<Socket> | null = null;
  let inbound: Buffer = Buffer.alloc(0);
  let nextRequestId = 1;
  let expectedSeq = 1;
  let closed = false;
  let fatalError: ErrorBody | null = null;
  let netModule: Promise<typeof import('node:net')> | null = null;

  /**
   * 为什么动态 import：AF_UNIX 只有 Node 提供。这个模块会被打包进浏览器产物
   * （main.tsx 静态 import 它），静态 `import 'node:net'` 会让 roller 直接打包失败；
   * 而这条路在 WebView 里本来就走不到（那里走 tauri.ts）。动态加载 + vite 侧 external，
   * 让「不可能用到」的 Node 依赖不进入浏览器主包。
   */
  function loadNet(): Promise<typeof import('node:net')> {
    netModule ??= import('node:net');
    return netModule;
  }

  function notifyTransportError(error: ErrorBody): void {
    for (const handler of [...errorHandlers]) handler(error);
  }

  /**
   * 致命失败：先把错误记下来并拒绝所有在途请求，再关掉 socket。
   * 为什么不「先关再报」：关闭是副作用，报错才是事实；顺序反了会在 handler 里看到半个状态。
   */
  function fail(error: ErrorBody): void {
    if (fatalError === null) fatalError = error;
    closed = true;
    const inFlight = [...pending.values()];
    pending.clear();
    for (const entry of inFlight) entry.reject(error);
    notifyTransportError(error);
    const doomed = socket;
    socket = null;
    connecting = null;
    doomed?.destroy();
  }

  function ensureSocket(): Promise<Socket> {
    if (fatalError !== null) return Promise.reject(fatalError);
    if (closed) return Promise.reject(errorBody('io', '客户端已关闭，不再接受新请求'));
    if (socket !== null) return Promise.resolve(socket);
    if (connecting !== null) return connecting;

    connecting = (async (): Promise<Socket> => {
      let net: typeof import('node:net');
      try {
        net = await loadNet();
      } catch (cause) {
        // 非 Node 环境（WebView/浏览器）里拿到的是模块解析失败：如实报 io，不假装连上了。
        const body = errorBody('io', '当前运行环境没有 Node 的 AF_UNIX 支持，请改用 Tauri 通道', {
          cause: cause instanceof Error ? cause.message : String(cause),
        });
        fail(body);
        throw body;
      }
      return new Promise<Socket>((resolve, reject) => {
        const candidate = net.connect({ path: socketPath });
        candidate.on('connect', () => {
          socket = candidate;
          connecting = null;
          resolve(candidate);
        });
        candidate.on('data', (chunk: Buffer) => {
          onData(chunk);
        });
        candidate.on('error', (cause: Error) => {
          const errno = (cause as { code?: unknown }).code;
          const body = errorBody('io', `AF_UNIX 连接失败：${cause.message}`, {
            path: socketPath,
            cause: typeof errno === 'string' ? errno : 'unknown',
          });
          fail(body);
          reject(body);
        });
        candidate.on('close', () => {
          // 只有「不是我们主动关、也没有更早的致命错误」时，断开才是新事实。
          if (!closed && fatalError === null) {
            fail(errorBody('io', 'daemon 关闭了 AF_UNIX 连接'));
          }
        });
      });
    })();
    return connecting;
  }

  function dispatch(parsed: unknown): boolean {
    if (typeof parsed !== 'object' || parsed === null) {
      fail(errorBody('internal', '收到不是对象的帧', { received: String(parsed) }));
      return false;
    }
    const frame = parsed as Partial<Frame> & { kind?: unknown };

    if (frame.kind === 'response') {
      const responseFrame = frame as Extract<Frame, { kind: 'response' }>;
      const entry = pending.get(responseFrame.id);
      if (entry === undefined) {
        fail(errorBody('internal', `收到未知请求 id 的应答`, { id: responseFrame.id }));
        return false;
      }
      pending.delete(responseFrame.id);
      if (responseFrame.outcome.status === 'ok') {
        entry.resolve(responseFrame.outcome.response);
      } else {
        entry.reject(responseFrame.outcome.error);
      }
      return true;
    }

    if (frame.kind === 'event') {
      const eventFrame = frame as Extract<Frame, { kind: 'event' }>;
      if (eventFrame.seq !== expectedSeq) {
        // 跳号 = 有帧丢了。静默继续会让界面显示一份「看起来完整」的假序列。
        fail(
          errorBody('internal', `事件 seq 跳号：期望 ${expectedSeq}，收到 ${eventFrame.seq}`, {
            expected: expectedSeq,
            received: eventFrame.seq,
          }),
        );
        return false;
      }
      expectedSeq += 1;
      for (const handler of [...eventHandlers]) handler(eventFrame.event);
      return true;
    }

    fail(errorBody('internal', `未知帧 kind：${String(frame.kind)}`));
    return false;
  }

  /**
   * 半帧处理：长度前缀说还要等，就**直接返回**，让内核把剩下的字节当事件送上来。
   * 这里没有 sleep、没有轮询、没有「等一会儿再读」—— 数据到达本身就是事件。
   */
  function onData(chunk: Buffer): void {
    inbound = inbound.length === 0 ? chunk : Buffer.concat([inbound, chunk]);
    while (inbound.length >= 4) {
      const length = inbound.readUInt32BE(0);
      if (length > maxFrameBytes) {
        fail(errorBody('invalid_request', `帧长度 ${length} 超过上限 ${maxFrameBytes}`, { length, maxFrameBytes }));
        return;
      }
      if (inbound.length < 4 + length) return;
      const text = inbound.subarray(4, 4 + length).toString('utf8');
      inbound = inbound.subarray(4 + length);
      let parsed: unknown;
      try {
        parsed = JSON.parse(text);
      } catch (cause) {
        fail(
          errorBody('internal', '帧内容不是合法 JSON', {
            raw: text.slice(0, 256),
            cause: cause instanceof Error ? cause.message : String(cause),
          }),
        );
        return;
      }
      if (!dispatch(parsed)) return;
    }
  }

  function writeFrame(target: Socket, frame: Frame, onWriteError: (error: ErrorBody) => void): void {
    const payload = Buffer.from(JSON.stringify(frame), 'utf8');
    if (payload.length > maxFrameBytes) {
      onWriteError(errorBody('invalid_request', `请求帧 ${payload.length} 字节超过上限 ${maxFrameBytes}`));
      return;
    }
    const header = Buffer.alloc(4);
    header.writeUInt32BE(payload.length, 0);
    target.write(Buffer.concat([header, payload]), (cause?: Error | null) => {
      if (cause !== undefined && cause !== null) {
        onWriteError(errorBody('io', `写帧失败：${cause.message}`));
      }
    });
  }

  function request(message: Request): Promise<Response> {
    const id = nextRequestId;
    nextRequestId += 1;
    const frame: Frame = { kind: 'request', id, request: message };
    return new Promise<Response>((resolve, reject) => {
      void ensureSocket().then(
        (target) => {
          pending.set(id, { resolve, reject });
          writeFrame(target, frame, (error) => {
            pending.delete(id);
            reject(error);
          });
        },
        (error: ErrorBody) => reject(error),
      );
    });
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
        // daemon 没订上的主题永远不会来事件；当成成功会让界面永远停在「没有日志/没有状态」的假象里。
        throw errorBody('internal', `daemon 未受理这些主题：${missing.join(', ')}`, { requested: topics, granted: granted.topics });
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
      // 关闭是用户/调用者的显式动作，不是传输失败：不通知 onTransportError，但要如实拒绝在途请求。
      const body = errorBody('io', '客户端已关闭，未完成的请求全部失败');
      const inFlight = [...pending.values()];
      pending.clear();
      for (const entry of inFlight) entry.reject(body);
      const doomed = socket;
      socket = null;
      connecting = null;
      doomed?.destroy();
    },
  };
}
