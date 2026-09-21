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
import type { RecoveryState } from "./types";
import { isCount, isObject, isText, rejectPayload } from "./eventGuards";
import type { AppSnapshot, LogEntry, ProbeResult } from "./types";

/** 日志在内存里保留的上限。后端也有自己的上限，这里再兜一层防止长跑占用内存。 */
const MAX_UI_LOGS = 1500;

/** 「已自动恢复」提示展示多久（可感知的结束，但不长期占位）。 */
const RECOVERED_NOTICE_MS = 8000;



/**
 * 日志**读取**状态（区别于「读到了，但是空的」）。
 *
 * **只由后端返回值决定**：IPC 成功就是 loaded，抛错就是 failed ——
 * 不按时间、不按次数、也不按「列表是不是空的」猜。这是 task-23 的硬要求：
 * 日志页曾经把「读不到」显示成「核心还没启动过」，而且给的是**错误的原因**。
 *
 * 界面用它 + `snapshot.runtime.running` 派生出三种互不相同的说法：
 *   · `failed`                    → 读取失败：显示后端给的原文 + 重试
 *   · `loaded` + 空 + 核心没在跑   → 核心还没启动过
 *   · `loaded` + 空 + 核心在跑     → 核心已启动，但还没产出日志
 */
export interface LogsLoad {
  phase: "loading" | "loaded" | "failed";
  /** 失败原因（后端或传输层给的原文）；成功时为 null。 */
  error: string | null;
}

interface StoreValue {
  snapshot: AppSnapshot | null;
  logs: LogEntry[];
  /** 日志读取本身的状态 —— 「没读到」与「读到但是空的」是两件事。 */
  logsLoad: LogsLoad;
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
  clearLogs: () => Promise<void>;
  /** 重新拉一次日志（失败态里的「重试」）。 */
  reloadLogs: () => Promise<void>;
}

const StoreContext = createContext<StoreValue | null>(null);

export function StoreProvider({ children }: { children: ReactNode }) {
  const [snapshot, setSnapshot] = useState<AppSnapshot | null>(null);
  const [logs, setLogs] = useState<LogEntry[]>([]);
  /** 日志读取状态：最初是「正在读」，成败由 IPC 的真实结果决定。 */
  const [logsLoad, setLogsLoad] = useState<LogsLoad>({ phase: "loading", error: null });
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
  //   只有一件：「刚恢复成功」的一次性提示（可感知的结束，8 秒后自己消失）。
  //   曾经还有一条「载荷里没有 recovery 就补拉快照读 notice」的兼容兜底 ——
  //   后端已下发 `runtime.recovery`，那条**已删除**（它会让每次 running 跳变都多拉一次完整快照）。
  const [recoveredAttempt, setRecoveredAttempt] = useState<number | null>(null);
  const prevRecovering = useRef<boolean | null>(null);
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
  // ---- 拉取历史日志（后端保留了进程启动以来的全部日志） ----
  //
  // 这里以前是 `.catch(() => { /* 日志拿不到不影响主功能，静默即可 */ })`。
  // 静默的代价是：**读失败和「真的没有日志」在界面上长得一模一样**，
  // 而日志页的文案直接把空列表解释成「核心还没启动过」—— 于是用户被告知了一个
  // 错误的原因。这属于本项目反复修过的「查不到 ≠ 没有」（流量、连接数、域名配对
  // 之后这是第四处），而且比前几处更糟：前几处只是没数据，这里是**给错因**。
  //
  // 现在失败必须留下痕迹：记下真实错误，由日志页显示原因并给出重试。
  const loadLogs = useCallback(async () => {
    setLogsLoad({ phase: "loading", error: null });
    try {
      const entries = await api.tailLogs(500);
      // 事件推送可能已经先到了：已有内容时不要用历史覆盖实时。
      setLogs((prev) => (prev.length ? prev : entries));
      setLogsLoad({ phase: "loaded", error: null });
    } catch (e) {
      setLogsLoad({ phase: "failed", error: errorText(e) });
    }
  }, []);

  useEffect(() => {
    void loadLogs();
  }, [loadLogs]);

  const clearLogs = useCallback(async () => {
    try {
      await api.clearLogs();
      // **只在后端确认删掉之后**才清空界面。反过来先清界面的话，清空失败时
      // 界面会显示「空的」而日志文件还在 —— 刷新一次日志全回来，
      // 那是我们自己制造「说的与事实不符」。
      setLogs([]);
      setLogsLoad({ phase: "loaded", error: null });
    } catch (e) {
      setError(`清空日志失败：${errorText(e)}`);
    }
  }, []);

  /** 从快照派生：恢复状态是 `runtime` 的一部分，事件与刷新两条路都会更新它。 */
  const recovery = useMemo(() => parseRecovery(snapshot?.runtime), [snapshot]);

  const value = useMemo<StoreValue>(
    () => ({
      snapshot,
      logs,
      logsLoad,
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
      reloadLogs: loadLogs,
    }),
    [
      snapshot,
      logs,
      logsLoad,
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
      loadLogs,
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
