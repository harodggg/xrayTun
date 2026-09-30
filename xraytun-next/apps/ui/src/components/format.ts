// 界面层的格式化与文案。
//
// 为什么单独一个文件：这里最容易出现「顺手补个默认值」的诱惑。
// 本项目的界面宪法是「未知就是未知」，所以本文件里没有一个函数会把
// 缺失值变成 0 / '-' / '0 B'；缺失由调用方渲染成「未采样 / 未知」。

import type {
  Capability,
  ConnectPhase,
  DaemonHello,
  LogLevel,
  RunMode,
  Stage,
} from '../transport/contract';

// stage 的中文文案。只在这里出现一次，避免各页面各写一套。
export const STAGE_TEXT: Record<Stage, string> = {
  disconnected: '未连接',
  connecting: '连接中',
  connected: '已连接',
  disconnecting: '断开中',
};

// Connecting 的子阶段：用户需要知道卡在哪一步，而不是盯着一个转圈图标。
// 文案由 ux 的 docs/ux/INTERACTION.md 逐字规定，改这里必须同步改验收测试。
export const PHASE_TEXT: Record<ConnectPhase, string> = {
  preparing_config: '正在生成配置',
  starting_core: '正在启动核心进程',
  awaiting_ready: '正在等待核心可连',
  committing_routes: '正在提交路由',
};

// 本轮只实现了 proxy 模式；tun 需要特权 helper（未实现），所以界面上
// 永远不会提供一个假的「TUN 开关」。这个映射只用于如实显示 wire 上已有的值。
export const MODE_TEXT: Record<RunMode, string> = {
  proxy: '代理模式（SOCKS 入站）',
  tun: 'TUN 模式',
};

export const LOG_LEVELS: LogLevel[] = ['error', 'warn', 'info', 'debug'];

// 日志级别的中文文案：色盲用户与「只靠颜色」的场景都需要文字，颜色只是补充。
export const LOG_LEVEL_TEXT: Record<LogLevel, string> = {
  error: '错误',
  warn: '警告',
  info: '信息',
  debug: '调试',
};

// 能力宣告 = 唯一事实源：daemon 没宣告的能力，界面不渲染入口（也不渲染灰按钮）。
// hello 还没到时 capabilities 未知，同样不渲染 —— 未知不等于有。
export function hasCapability(hello: DaemonHello | null | undefined, capability: Capability): boolean {
  return hello != null && hello.capabilities.includes(capability);
}

// 统一的失败展示形状。code 为 null 表示「拿不到契约里的 ErrorCode」——
// 这时不能编一个 code 出来，只能如实省略。
export interface DisplayError {
  code: string | null;
  message: string;
}

export function toDisplayError(error: unknown): DisplayError {
  if (error != null && typeof error === 'object') {
    const record = error as { code?: unknown; message?: unknown };
    if (typeof record.message === 'string') {
      return {
        code: typeof record.code === 'string' ? record.code : null,
        message: record.message,
      };
    }
  }
  if (error instanceof Error) {
    return { code: null, message: error.message };
  }
  return { code: null, message: String(error) };
}

// 契约里的 ErrorBody 与传输层抛出的错误都可能出现在这里，
// 参数故意用 unknown：不逼调用方断言类型，也不假设错误一定长成 ErrorBody。
export function toDisplayErrorBody(body: unknown): DisplayError | null {
  if (body == null) return null;
  return toDisplayError(body);
}

// 人类可读字节数。只对真实采样值调用；未采样由调用方显示「未采样」。
// 显示时另附原始字节数，保证格式化的结果仍然是可追溯的忠实表示。
export function formatBytes(bytes: number): string {
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  if (unit === 0) return `${value} B`;
  return `${value.toFixed(1)} ${units[unit]}`;
}

// epoch ms → 本地时间字符串。手写而不是 toLocaleString：
// 验收测试需要稳定的输出，不能随运行环境的 locale 变化。
export function formatTime(ms: number): string {
  if (!Number.isFinite(ms)) return '未知';
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
