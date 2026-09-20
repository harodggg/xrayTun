/**
 * 应用状态容器。
 *
 * 设计取舍：**单一快照 + 全量刷新**，而不是细粒度的局部状态。
 *
 * 后端每个会改变状态的命令都返回完整 `AppSnapshot`，前端直接整体替换。
 * 好处是永远不会出现「节点列表更新了但选中项没更新」这类不一致；
 * 代价是每次刷新会重建对象 —— 但这个应用的数据量（几百个节点）下，
 * React 的 diff 成本完全可以忽略，而一致性 bug 的成本高得多。
 *
 * 事件推送（日志、流量、延迟）走增量更新，因为它们频率高且不影响快照一致性。
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { api, errorText, parseRecovery, subscribe } from "./ipc";
import type { RecoveryState } from "./ipc";
import { isCount, isObject, isText, rejectPayload } from "./eventGuards";
import type { AppSnapshot, LogEntry, ProbeResult } from "./types";

/** 日志在内存里保留的上限。后端也有自己的上限，这里再兜一层防止长跑占用内存。 */
const MAX_UI_LOGS = 1500;

/** 「已自动恢复」提示展示多久（可感知的结束，但不长期占位）。 */
const RECOVERED_NOTICE_MS = 8000;



interface StoreValue {
  snapshot: AppSnapshot | null;
  logs: LogEntry[];
  /** 正在执行的操作名，用于按钮转圈与防重复点击。 */
  busy: string | null;
  error: string | null;
  /** 延迟测量是否正在进行。 */
  probing: boolean;
  probeProgress: { done: number; total: number } | null;
  /**
   * 看门狗自愈状态（结构化，来自后端 `runtime.recovery`；没有就是 null）。
   *
   * 界面**只据它**判断「是否正在恢复」，绝不解析 notice 文案、也不按时间猜 ——
   * 见 `ipc.ts` 的 `recoveryView` 与 task-22。
   */
  recovery: RecoveryState | null;
  /** 刚自动恢复成功（第几次）；8 秒后自动清空。用于「可感知的结束」。 */
  recoveredAttempt: number | null;
  dismissRecovered: () => void;
  refresh: () => Promise<void>;
  /** 执行一个会返回新快照的操作。 */
  run: (name: string, action: () => Promise<AppSnapshot>) => Promise<boolean>;
  /** 执行一个不返回快照的操作。 */
  runVoid: (name: string, action: () => Promise<void>) => Promise<boolean>;
  clearError: () => void;
  clearLogs: () => void;
}

const StoreContext = createContext<StoreValue | null>(null);

export function StoreProvider({ children }: { children: ReactNode }) {
  const [snapshot, setSnapshot] = useState<AppSnapshot | null>(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [probing, setProbing] = useState(false);
  const [probeProgress, setProbeProgress] = useState<{ done: number; total: number } | null>(null);

  // 用 ref 保存 busy 的最新值：事件回调里需要判断「是否正在忙」，
  // 但它不应该成为 useCallback 的依赖（否则会重建所有回调）。
  const busyRef = useRef<string | null>(null);
  busyRef.current = busy;

  // ---- 自动恢复（task-22）------------------------------------------------
  //
  // 恢复状态本身**不单独存状态**：后端把它放在 `runtime.recovery` 里，而
  // `onRuntime` 与 `refresh()` 两条路都会整体更新 `snapshot.runtime` ——
  // 所以从 snapshot 派生即可，事件与快照天然一致、刷新也不会丢。
  // 这里只额外维护两件 snapshot 表达不了的事：
  //   1) 「刚恢复成功」的一次性提示（可感知的结束，8 秒后自己消失）；
  //   2) 兼容期兜底：载荷里还没有 recovery 时，靠一次快照把 notice 读出来。
  const [recoveredAttempt, setRecoveredAttempt] = useState<number | null>(null);
  const prevRecovering = useRef<boolean | null>(null);
  const prevRunning = useRef<boolean | null>(null);
  const recoveredTimer = useRef<number | null>(null);

  const dismissRecovered = useCallback(() => {
    if (recoveredTimer.current !== null) window.clearTimeout(recoveredTimer.current);
    recoveredTimer.current = null;
    setRecoveredAttempt(null);
  }, []);

  useEffect(() => () => {
    if (recoveredTimer.current !== null) window.clearTimeout(recoveredTimer.current);
  }, []);

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await api.snapshot());
      setError(null);
    } catch (e) {
      setError(errorText(e));
    }
  }, []);

  const run = useCallback(
    async (name: string, action: () => Promise<AppSnapshot>): Promise<boolean> => {
      if (busyRef.current) return false;
      setBusy(name);
      setError(null);
      try {
        setSnapshot(await action());
        return true;
      } catch (e) {
        setError(errorText(e));
        return false;
      } finally {
        setBusy(null);
      }
    },
    [],
  );

  const runVoid = useCallback(
    async (name: string, action: () => Promise<void>): Promise<boolean> => {
      if (busyRef.current) return false;
      setBusy(name);
      setError(null);
      try {
        await action();
        return true;
      } catch (e) {
        setError(errorText(e));
        return false;
      } finally {
        setBusy(null);
      }
    },
    [],
  );

  // ---- 初始加载 + 事件订阅 ----
  useEffect(() => {
    void refresh();

    const unsubscribe = subscribe({
      onRuntime: (payload) => {
        if (!isObject(payload) || !isObject(payload.runtime) || !isObject(payload.traffic)) {
          return rejectPayload("runtime://changed", payload, "缺少 runtime/traffic 对象");
        }
        // 恢复状态是**结构化**的（在 runtime.recovery 里）。畸形就当作没有 —— 不猜。
        const rec = parseRecovery(payload.runtime);
        const runningNow = payload.runtime.running === true;

        // 结束必须「可感知」：从「正在恢复」变成「不在恢复」且结局是成功时，
        // 给一次性提示。没有它，用户只会看到界面悄悄变回「已连接」。
        const wasRecovering = prevRecovering.current;
        prevRecovering.current = rec ? rec.recovering : null;
        if (wasRecovering === true && rec && !rec.recovering && rec.last_outcome === "recovered") {
          setRecoveredAttempt(rec.attempt);
          if (recoveredTimer.current !== null) window.clearTimeout(recoveredTimer.current);
          recoveredTimer.current = window.setTimeout(() => {
            recoveredTimer.current = null;
            setRecoveredAttempt(null);
          }, RECOVERED_NOTICE_MS);
        } else if (rec && (rec.recovering || rec.last_outcome === "direct_fallback")) {
          // **不能出现自相矛盾的同屏**：又开始了新一次恢复、或这次失败了，
          // 上一次那条「已自动恢复」就必须立刻收掉 —— 否则「已恢复」会和
          // 「正在恢复」/「恢复失败」同时挂着（实测复现过）。
          if (recoveredTimer.current !== null) window.clearTimeout(recoveredTimer.current);
          recoveredTimer.current = null;
          setRecoveredAttempt(null);
        }

        // ---- 兼容期兜底（backend 落地 `runtime.recovery` 后应删掉）----------
        // 字段还没下发时，唯一能看到看门狗状态的地方是 `snapshot.notice`，而它只走
        // snapshot 命令。所以在「从在跑到没在跑」这一次跳变时补拉一次快照。
        // 只在跳变时拉，不是每次事件都拉（快照组装要读文件、问核心版本，很贵）。
        const wasRunning = prevRunning.current;
        prevRunning.current = runningNow;
        if (!rec && wasRunning === true && !runningNow) void refresh();

        // 增量更新运行时与流量：这两个字段高频变化，全量刷新会很浪费。
        setSnapshot((prev) =>
          prev
            ? {
                ...prev,
                runtime: payload.runtime as typeof prev.runtime,
                traffic: payload.traffic as typeof prev.traffic,
              }
            : prev,
        );
      },
      onUpdateProgress: (payload) => {
        if (
          !isObject(payload) ||
          !isText(payload.label) ||
          !isCount(payload.done_bytes) ||
          !isCount(payload.total_bytes)
        ) {
          return rejectPayload("update://progress", payload, "进度字段不完整");
        }
        // 进度只更新这一个字段：下载期间每 200ms 一次，全量刷新太浪费。
        setSnapshot((prev) =>
          prev
            ? {
                ...prev,
                update: {
                  ...prev.update,
                  progress: {
                    label: payload.label,
                    done_bytes: payload.done_bytes,
                    total_bytes: payload.total_bytes,
                  },
                },
              }
            : prev,
        );
      },
      onLog: (payload) => {
        if (!isObject(payload) || !isText(payload.line) || !isText(payload.level)) {
          return rejectPayload("core://log", payload, "缺少 line/level 字符串");
        }
        setLogs((prev) => {
          const next = [
            ...prev,
            {
              ts_unix: Math.floor(Date.now() / 1000),
              source: "core",
              level: payload.level,
              message: payload.line,
            },
          ];
          return next.length > MAX_UI_LOGS ? next.slice(next.length - MAX_UI_LOGS) : next;
        });
      },
      onLatency: (results) => {
        // **这一条实测崩过**：载荷不是数组时 `for...of` 会抛错并卸载整个树。
        if (!Array.isArray(results)) {
          return rejectPayload("nodes://latency", results, "载荷不是数组");
        }
        setSnapshot((prev) => {
          if (!prev) return prev;
          const latency = { ...prev.latency };
          for (const r of results as ProbeResult[]) {
            if (r && isText(r.node_id)) latency[r.node_id] = r;
          }
          return { ...prev, latency };
        });
        setProbing(false);
        setProbeProgress(null);
      },
      onProbeStarted: (payload) => {
        if (!isObject(payload) || !isCount(payload.total)) {
          return rejectPayload("nodes://probe-started", payload, "缺少 total");
        }
        setProbing(true);
        setProbeProgress({ done: 0, total: payload.total });
      },
      onNodesChanged: () => void refresh(),
      onSubscriptionsChanged: () => void refresh(),
      onSettingsChanged: () => void refresh(),
    });

    return unsubscribe;
  }, [refresh]);

  // ---- 拉取历史日志（后端保留了进程启动以来的全部日志） ----
  useEffect(() => {
    void api
      .tailLogs(500)
      .then((entries) => setLogs((prev) => (prev.length ? prev : entries)))
      .catch(() => {
        /* 日志拿不到不影响主功能，静默即可 */
      });
  }, []);

  const clearLogs = useCallback(() => {
    setLogs([]);
    void api.clearLogs().catch(() => undefined);
  }, []);

  /** 从快照派生：恢复状态是 `runtime` 的一部分，事件与刷新两条路都会更新它。 */
  const recovery = useMemo(() => parseRecovery(snapshot?.runtime), [snapshot]);

  const value = useMemo<StoreValue>(
    () => ({
      snapshot,
      logs,
      busy,
      error,
      probing,
      probeProgress,
      recovery,
      recoveredAttempt,
      dismissRecovered,
      refresh,
      run,
      runVoid,
      clearError: () => setError(null),
      clearLogs,
    }),
    [
      snapshot,
      logs,
      busy,
      error,
      probing,
      probeProgress,
      recovery,
      recoveredAttempt,
      dismissRecovered,
      refresh,
      run,
      runVoid,
      clearLogs,
    ],
  );

  return <StoreContext.Provider value={value}>{children}</StoreContext.Provider>;
}

export function useStore(): StoreValue {
  const ctx = useContext(StoreContext);
  if (!ctx) {
    throw new Error("useStore 必须在 StoreProvider 内使用");
  }
  return ctx;
}
