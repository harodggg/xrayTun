// xraytun-next · 前端契约镜像（唯一事实源：crates/xt-contract/src/{error,model,protocol}.rs）
//
// 为什么手写镜像而不是代码生成：本轮只有一份契约、一个消费者，生成器带来的构建步骤与
// 依赖比它省下的几十行更贵；代价是「忘了同步」，所以配了 contract.guard.test.ts 做
// 文本级字段对照（不是类型级 —— 见该文件头部写明的覆盖边界）。
//
// 与冻结清单的两处刻意差异（写在这里，别让下一个人猜）：
//  1. `capabilities` 用 `Capability[]` 而不是 `string[]`：Rust 是 `Vec<Capability>`
//     （snake_case 枚举）。放宽成 `string[]` 只会把拼写错误留到运行时；收窄不可能
//     让任何读取方编译失败。
//  2. 追加了 Request/Response/Outcome/Frame 四个**线上帧**类型：它们同样逐字段来自
//     protocol.rs，unixSocket.ts 的编解码需要它们。冻结清单只列了领域视图部分。
//
// 通用规则：Rust 侧 `Option<T>` + `skip_serializing_if` → TS 侧可选字段 `?:`，
// 不用 `| null` —— 线上根本不会出现显式 null，见到字段缺失就是「不知道」。

// NodeId / SubscriptionId 在 Rust 是 String newtype，线上就是裸字符串。
// 这里用别名而不是 branded type：brand 会凭空造出一个线上不存在的约束，
// 并让普通 string 实参无法传入。
export type NodeId = string;
export type SubscriptionId = string;

export type Stage = 'disconnected' | 'connecting' | 'connected' | 'disconnecting';
export type ConnectPhase = 'preparing_config' | 'starting_core' | 'awaiting_ready' | 'committing_routes';
export type RunMode = 'proxy' | 'tun';
export type LogLevel = 'error' | 'warn' | 'info' | 'debug';
export type ErrorCode =
  | 'invalid_request'
  | 'not_found'
  | 'conflict'
  | 'permission_denied'
  | 'datapath_unavailable'
  | 'config_invalid'
  | 'core_exited_early'
  | 'helper_unavailable'
  | 'io'
  | 'internal'
  // 「本版本不提供该能力」（对应 capabilities 未宣告）。它只能表示「没做这个能力」，
  // 不能用来包装「试了但失败」——后者有各自的失败分类。
  | 'unsupported';

export interface ErrorBody {
  code: ErrorCode;
  message: string;
  detail?: unknown;
}

export interface DatapathView {
  pid?: number;
  version?: string;
  ready_at_ms?: number;
}

export interface StatsView {
  uplink_bytes: number;
  downlink_bytes: number;
  sampled_at_ms: number;
}

export interface ConnectionView {
  stage: Stage;
  phase?: ConnectPhase;
  mode?: RunMode;
  node_id?: NodeId;
  connected_since_ms?: number;
  datapath: DatapathView;
  stats?: StatsView;
  last_error?: ErrorBody;
}

export type NodeSource = { kind: 'subscription'; id: SubscriptionId } | { kind: 'manual' };

export interface ProbeResult {
  node_id: NodeId;
  ttfb_ms?: number;
  error?: ErrorBody;
  at_ms: number;
}

export interface NodeView {
  id: NodeId;
  name: string;
  protocol: string;
  endpoint: string;
  source: NodeSource;
  probe?: ProbeResult;
}

export interface LogLine {
  ts_ms: number;
  level: LogLevel;
  target: string;
  message: string;
}

export type NoticeSeverity = 'info' | 'warning' | 'error';

export interface Notice {
  severity: NoticeSeverity;
  code: ErrorCode;
  message: string;
  at_ms: number;
}

export interface SubscriptionView {
  id: SubscriptionId;
  url: string;
  node_count: number;
  fetched_at_ms?: number;
  last_error?: ErrorBody;
}

export interface SettingsView {
  socks_listen: string;
  selected_node?: NodeId;
  log_level: LogLevel;
}

// 局部更新：字段缺失 = 不改这一项（不是「设成 null」）。
export interface SettingsPatch {
  socks_listen?: string;
  selected_node?: NodeId;
  log_level?: LogLevel;
}

/**
 * Rust `Capability`（snake_case 枚举）。它出现在 DaemonHello.capabilities 里，
 * 是**能力事实源**：界面只渲染已宣告能力的入口，store 原样透出、不做本地推断。
 * `subscriptions` = 解析本地订阅原文并列出节点；`subscription_fetch` = http/https 拉取刷新。
 */
export type Capability =
  | 'proxy_mode'
  | 'tun_mode'
  | 'stats'
  | 'probe'
  | 'subscriptions'
  | 'subscription_fetch';

export interface DaemonHello {
  daemon_version: string;
  protocol_version: number;
  capabilities: Capability[];
  pid: number;
  started_at_ms: number;
}

export type Topic = 'state' | 'log' | 'probe' | 'notice';

export const ALL_TOPICS: readonly Topic[] = ['state', 'log', 'probe', 'notice'];

export type DaemonEvent =
  | { event: 'state'; view: ConnectionView }
  | { event: 'log'; line: LogLine }
  | { event: 'probe'; result: ProbeResult }
  | { event: 'notice'; notice: Notice };

// ---------------------------------------------------------------- 线上帧（protocol.rs）
//
// 帧格式由 xt-ipc 负责：4 字节大端长度前缀 + 一帧 UTF-8 JSON。
// 判别式是 `kind`，请求 id 与事件 seq 都是 u64 → JS number（2^53 以内的整数）。

export type RequestId = number;
export type EventSeq = number;

export type Request =
  | { op: 'hello'; client_version: string; protocol_version: number }
  | { op: 'subscribe'; topics: Topic[] }
  | { op: 'status' }
  | { op: 'connect'; node_id: NodeId; mode: RunMode }
  | { op: 'disconnect' }
  | { op: 'switch_node'; node_id: NodeId }
  | { op: 'list_nodes' }
  | { op: 'probe_nodes'; node_ids: NodeId[] }
  | { op: 'get_settings' }
  | { op: 'patch_settings'; patch: SettingsPatch }
  | { op: 'list_subscriptions' }
  | { op: 'add_subscription'; url: string }
  | { op: 'refresh_subscription'; id: SubscriptionId }
  | { op: 'tail_logs'; lines: number };

// `nodes` / `subscriptions` / `logs` 三个载荷的键名不是我们挑的：Rust 侧原本是
// `Nodes(Vec<NodeView>)` 这类「内部 tag + newtype 包序列」—— serde 对那种形状会在运行时直接报错，
// 现已改成结构体变体 `Nodes { nodes: Vec<NodeView> }` 等（lead 修，并有
// crates/xt-contract/tests/wire.rs 做 JSON round-trip + 具名键断言）。
// contract.guard.test.ts 里有一条专门盯这三个键名的断言：形状再变就会红。
export type Response =
  | ({ result: 'hello' } & DaemonHello)
  | { result: 'subscribed'; topics: Topic[] }
  | ({ result: 'status' } & ConnectionView)
  | { result: 'nodes'; nodes: NodeView[] }
  | ({ result: 'settings' } & SettingsView)
  | { result: 'subscriptions'; subscriptions: SubscriptionView[] }
  | { result: 'logs'; logs: LogLine[] }
  // 受理：终态一定在事件流里出现（成功或失败都出现，不会石沉大海）。
  | { result: 'accepted' }
  // 同步成功，没有需要等待的结果。
  | { result: 'ok' };

export type Outcome = { status: 'ok'; response: Response } | { status: 'error'; error: ErrorBody };

export type Frame =
  | { kind: 'request'; id: RequestId; request: Request }
  | { kind: 'response'; id: RequestId; outcome: Outcome }
  | { kind: 'event'; seq: EventSeq; event: DaemonEvent };

// ---------------------------------------------------------------- 常量（lib.rs）

/** 线上协议版本。版本不符必须被明确拒绝，不允许「尽量兼容」。 */
export const PROTOCOL_VERSION = 1;

/** 单帧上限（1 MiB），超过即拒绝：帧大小失控通常是 bug，不是需求。 */
export const MAX_FRAME_BYTES = 1024 * 1024;

/** 客户端自己的版本号，只在 hello 里如实上报，没有其他用途。 */
export const CLIENT_VERSION = '0.0.0';
