// xraytun-next · DaemonClient 冻结接口
//
// 为什么接口长这样：UI 只有一条通往 daemon 的路（AF_UNIX + 契约帧，见 00-CONTRACT-FREEZE.md §3），
// 传输实现有两份（unixSocket.ts 给活体验收与无头调试、tauri.ts 给打包后的应用），
// store 只认这个接口 —— 于是「换传输」不需要动界面，也不需要任何编译期分支。
//
// 两条硬规则落在签名上（不是注释里的纪律）：
//  * 受理/终态分离（I1）：connect/disconnect/switchNode/probeNodes 受理即返回，
//    终态**只**通过 onEvent 到达。没有「等一会儿再问一次」的余地。
//  * 失败如实抛出（I2）：reject 的值就是契约里的 ErrorBody 本体（`{code, message, detail?}`），
//    没有包装层、没有降级返回值、没有「失败就返回默认值」。
//
// 运行时事实（属架构文档的「未验证/限制」清单）：unixSocket.ts 需要 Node 的 AF_UNIX，
// **非 Tauri 的浏览器/WebView 环境里这条通道不可用** —— 它不会假装连上，而是以
// `transportError`（code `io`）失败；那种环境只能用 tauri.ts，而 tauri.ts 的真机状态
// 见它自己文件头的「未经 macOS 真机验证」。

import type {
  ConnectionView,
  DaemonEvent,
  DaemonHello,
  ErrorBody,
  ErrorCode,
  LogLine,
  NodeView,
  Response,
  RunMode,
  SettingsPatch,
  SettingsView,
  SubscriptionView,
  Topic,
} from './contract';

export interface DaemonClient {
  /** 一台连接上的第一个请求（协议要求）；daemon 身份的唯一事实源。 */
  hello(): Promise<DaemonHello>;
  /** 声明本次连接只收这些主题。返回后 onEvent 才会开始收到对应事件。 */
  subscribe(topics: Topic[]): Promise<void>;
  /**
   * 返回取消订阅函数。
   * 为什么是「注册函数集合」而不是「subscribe(topics, handler)」：主题过滤是 daemon 侧的责任
   * （少发就是省电），客户端只负责把收到的每一帧交给所有监听者 —— 两者不要混成一个参数。
   */
  onEvent(handler: (e: DaemonEvent) => void): () => void;
  /** 当前快照。它是「还没收到任何事件时」的唯一事实来源。 */
  status(): Promise<ConnectionView>;
  /** 受理连接意图；终态见 onEvent('state')。 */
  connect(nodeId: string, mode: RunMode): Promise<void>;
  disconnect(): Promise<void>;
  switchNode(nodeId: string): Promise<void>;
  listNodes(): Promise<NodeView[]>;
  /** 发起探测；每条结果以 onEvent('probe') 回来。 */
  probeNodes(ids: string[]): Promise<void>;
  getSettings(): Promise<SettingsView>;
  /** 局部更新（缺字段 = 不改）。应答没有新值，需要新值时另外 getSettings 一次。 */
  patchSettings(patch: SettingsPatch): Promise<void>;
  listSubscriptions(): Promise<SubscriptionView[]>;
  addSubscription(url: string): Promise<void>;
  refreshSubscription(id: string): Promise<void>;
  tailLogs(lines: number): Promise<LogLine[]>;
  /** 关闭传输。不等待、不重连：连接没了就没了（I2）。 */
  close(): void;

  /**
   * 传输层的**致命**失败通道（可选扩展，超出冻结清单的方法集）：
   * 事件 seq 跳号、帧解析失败、socket 断开这类「连接已不可信」的错误必须能到达 store，
   * 否则 DaemonState.transportError 永远无法为真 —— 静默丢弃等于撒谎（I3）。
   * 非致命失败（如 not_found）不走这里，它们通过各自调用的 reject 原样暴露给调用者。
   */
  onTransportError?(handler: (error: ErrorBody) => void): () => void;
}

/** 封闭的 ErrorCode 集合，用于把「任意抛出的东西」判成是否已经是 ErrorBody。 */
const ERROR_CODES: readonly string[] = [
  'invalid_request',
  'not_found',
  'conflict',
  'permission_denied',
  'datapath_unavailable',
  'config_invalid',
  'core_exited_early',
  'helper_unavailable',
  'io',
  'internal',
  'unsupported',
];

export function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === 'string' && ERROR_CODES.includes(value);
}

export function errorBody(code: ErrorCode, message: string, detail?: unknown): ErrorBody {
  return detail === undefined ? { code, message } : { code, message, detail };
}

/**
 * 把「任意被抛出的值」收敛成 ErrorBody。
 * 为什么需要它：JS 的 catch 能接到任何东西，而界面只认识契约里的错误形状；
 * 这里宁可给出 `internal`（我们的 bug）也不编一个更好看的分类。
 * 注意不能只看 `typeof code === 'string'`：Node 的 socket 错误自带
 * `code: 'ECONNREFUSED'`，那不是 ErrorCode 的成员。
 */
export function toErrorBody(error: unknown): ErrorBody {
  if (typeof error === 'object' && error !== null) {
    const candidate = error as { code?: unknown; message?: unknown };
    if (isErrorCode(candidate.code) && typeof candidate.message === 'string') {
      return error as ErrorBody;
    }
    if (error instanceof Error) {
      const errno = (error as { code?: unknown }).code;
      // Node 的 errno 是纯大写标识（ECONNREFUSED/ENOENT…）→ 系统 IO 失败。
      if (typeof errno === 'string' && /^[A-Z][A-Z0-9_]*$/.test(errno)) {
        return errorBody('io', error.message, { cause: errno });
      }
      return errorBody('internal', error.message, { name: error.name });
    }
    return errorBody('internal', '传输层抛出了非 Error 的值', { thrown: String(error) });
  }
  return errorBody('internal', `传输层抛出了非对象的值：${String(error)}`);
}

// ------------------------------------------------------------------ 应答解码（两个传输实现共用）
//
// 为什么放在这里而不是各写一份：两份传输必须对「什么样的应答算合法」给出完全相同的答案，
// 否则同一个 daemon 会在一条通道上被接受、在另一条上被拒绝 —— 那就是两条路径（I2）。

/** 判别式不符 = 解码错了或协议变了；两种都必须吵闹，不能挑一个「像的」用。 */
export function pickResponse<K extends Response['result']>(
  response: Response,
  result: K,
): Extract<Response, { result: K }> {
  if (response.result !== result) {
    throw errorBody('internal', `期望 ${result} 应答，收到 ${String((response as { result?: unknown }).result)}`, {
      expected: result,
      received: response,
    });
  }
  return response as Extract<Response, { result: K }>;
}

/**
 * 受理类应答：契约只保证 connect/disconnect/switch_node/probe_nodes 返回 `accepted`；
 * add_subscription / refresh_subscription 没规定（`accepted` 或 `ok` 都可能）。
 * 两者语义完全相同（都没有需要等待的同步结果），都接受并不构成「失败了换一条路」。
 */
export function pickSettledResponse(response: Response): void {
  if (response.result === 'accepted' || response.result === 'ok') return;
  throw errorBody('internal', `期望 accepted/ok 应答，收到 ${String((response as { result?: unknown }).result)}`);
}

/** 数组字段存在性检查：字段缺失时不能把它当空数组用 —— 那会把「没收到」显示成「没有」。 */
export function requireArrayField(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) {
    throw errorBody('internal', `应答字段 ${field} 不是数组`, {
      field,
      received: value === undefined ? 'missing' : typeof value,
    });
  }
  return value;
}
