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
import { api, errorText, subscribe } from "./ipc";
import type { AppSnapshot, LogEntry, ProbeResult } from "./types";

/** 日志在内存里保留的上限。后端也有自己的上限，这里再兜一层防止长跑占用内存。 */
const MAX_UI_LOGS = 1500;

interface StoreValue {
  snapshot: AppSnapshot | null;
  logs: LogEntry[];
  /** 正在执行的操作名，用于按钮转圈与防重复点击。 */
  busy: string | null;
  error: string | null;
  /** 延迟测量是否正在进行。 */
  probing: boolean;
  probeProgress: { done: number; total: number } | null;
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
        // 增量更新运行时与流量：这两个字段高频变化，全量刷新会很浪费。
        setSnapshot((prev) =>
          prev ? { ...prev, runtime: payload.runtime, traffic: payload.traffic } : prev,
        );
      },
      onUpdateProgress: (payload) => {
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
      onLatency: (results: ProbeResult[]) => {
        setSnapshot((prev) => {
          if (!prev) return prev;
          const latency = { ...prev.latency };
          for (const r of results) {
            if (r.node_id) latency[r.node_id] = r;
          }
          return { ...prev, latency };
        });
        setProbing(false);
        setProbeProgress(null);
      },
      onProbeStarted: (payload) => {
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

  const value = useMemo<StoreValue>(
    () => ({
      snapshot,
      logs,
      busy,
      error,
      probing,
      probeProgress,
      refresh,
      run,
      runVoid,
      clearError: () => setError(null),
      clearLogs,
    }),
    [snapshot, logs, busy, error, probing, probeProgress, refresh, run, runVoid, clearLogs],
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
