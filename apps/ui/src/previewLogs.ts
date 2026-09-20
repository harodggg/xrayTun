/**
 * 预览桥接 · 假日志（`tail_logs` 命令）。
 *
 * 前 120 条撑出可滚动区域，末尾几条覆盖 info/warn/error 与多行文本 ——
 * 「跟随滚动」只有在**真的来新日志**时才验证得了（见桥接里的 `__emitCoreLog`）。
 */

import type { LogEntry } from "./types";

const now = Math.floor(Date.now() / 1000);

/** 造一批日志：前 120 条用来撑出可滚动区域，末尾几条覆盖 info/warn/error 与多行文本。 */
export const MOCK_LOGS: LogEntry[] = [
  ...Array.from({ length: 120 }, (_, i) => ({
    ts_unix: now - 900 + i,
    source: i % 3 === 0 ? "core" : "app",
    level: "info",
    message: `预热日志 #${i}：填充滚动区域，用于验证「跟随」开关是否真的生效`,
  })),

  { ts_unix: now - 740, source: "app", level: "info", message: "正在切换到「香港 · REALITY 01」，需要重建隧道（几秒）" },
  { ts_unix: now - 736, source: "core", level: "info", message: "Xray 26.9.9 started" },
  { ts_unix: now - 735, source: "app", level: "info", message: "连通性检查通过：经节点 189ms（HTTP 204）" },
  { ts_unix: now - 420, source: "core", level: "warn", message: "failed to dial 198.51.100.7:2053: connection reset by peer" },
  {
    ts_unix: now - 60,
    source: "core",
    level: "error",
    message: "启动失败:\n    \"port\": 10808\n    已被占用（另一个代理工具在跑？）",
  },
];
