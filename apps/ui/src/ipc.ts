//! Tauri IPC 封装。
//!
//! **所有** `invoke` / `listen` 都必须经过这里，不允许在组件里直接调用。
//! 原因：命令名和事件名是字符串，拼错在 Tauri 里不会报错 —— 只会静静地
//! 拿不到数据。集中在一处至少让「改名」变成一次全局搜索。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  GlobeData,
  NodeExport,
  RecentConnections,
  RouteExplanation,
  Topology,
  AppSnapshot,
  AppSettings,
  CoreRuntime,
  HelperAvailability,
  LogEntry,
  ProbeResult,
  ProxyMode,
  TrafficSample,
} from "./types";

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

export const api = {
  snapshot: () => invoke<AppSnapshot>("snapshot"),

  saveSettings: (settings: AppSettings) =>
    invoke<AppSnapshot>("save_settings", { settings }),

  setMode: (mode: ProxyMode) => invoke<AppSnapshot>("set_mode", { mode }),

  start: () => invoke<AppSnapshot>("start_proxy"),
  stop: () => invoke<AppSnapshot>("stop_proxy"),

  selectNode: (nodeId: string) => invoke<AppSnapshot>("select_node", { nodeId }),
  addManualNode: (link: string) => invoke<AppSnapshot>("add_manual_node", { link }),
  deleteNode: (nodeId: string) => invoke<AppSnapshot>("delete_node", { nodeId }),

  addSubscription: (name: string, url: string) =>
    invoke<AppSnapshot>("add_subscription", { name, url }),
  removeSubscription: (subscriptionId: string) =>
    invoke<AppSnapshot>("remove_subscription", { subscriptionId }),
  refreshSubscriptions: (ids?: string[]) =>
    invoke<AppSnapshot>("refresh_subscriptions", { ids: ids ?? null }),

  /** 地球仪数据：本机与出口节点的地理位置（来自 ip-api.com）。 */
  globeData: () => invoke<GlobeData>("globe_data"),

  /** 网络流动拓扑：真实入口/规则链/出口 + 实测流量。 */
  routingTopology: () => invoke<Topology>("routing_topology"),
  /** 判定某个目的地会走哪条规则（真实规则 + 真实 geosite/geoip 数据）。 */
  explainDest: (dest: string) => invoke<RouteExplanation>("explain_dest", { dest }),

  /**
   * 最近连接（单连接可视化，见 `docs/ui/topology/CONNECTIONS.md`）。
   *
   * 参数全部可选，过滤也可以放到后端做；前端默认只取最近一批，
   * 再在本地做即时过滤（输入框每敲一个字都往返一次太浪费）。
   * `limit` 默认 200，上限 1000（后端环形缓冲容量）。
   */
  recentConnections: (
    opts: { inbound?: string; outbound?: string; domain?: string; limit?: number } = {},
  ) => invoke<RecentConnections>("recent_connections", opts),

  testLatency: (nodeIds?: string[]) =>
    invoke<AppSnapshot>("test_latency", { nodeIds: nodeIds ?? null }),

  probeHelper: () => invoke<HelperAvailability>("probe_helper"),
  installHelper: () => invoke<AppSnapshot>("install_helper"),
  restartHelper: () => invoke<AppSnapshot>("restart_helper"),
  uninstallHelper: () => invoke<AppSnapshot>("uninstall_helper"),
  restoreStale: () => invoke<AppSnapshot>("restore_stale"),

  tailLogs: (limit?: number) => invoke<LogEntry[]>("tail_logs", { limit: limit ?? null }),
  clearLogs: () => invoke<void>("clear_logs"),
  diagnostics: () => invoke<string>("diagnostics"),
  openDataDir: () => invoke<void>("open_data_dir"),
  setLaunchAtLogin: (enabled: boolean) =>
    invoke<AppSnapshot>("set_launch_at_login", { enabled }),
  openLoginItemSettings: () => invoke<void>("open_login_item_settings"),
  exportNode: (nodeId: string) => invoke<NodeExport>("export_node", { nodeId }),
  checkUpdates: () => invoke<AppSnapshot>("check_updates"),
  installCoreUpdate: () => invoke<AppSnapshot>("install_core_update"),
  installGeoUpdate: () => invoke<AppSnapshot>("install_geo_update"),
  revertManagedUpdate: () => invoke<AppSnapshot>("revert_managed_update"),
  checkAppUpdate: () => invoke<AppSnapshot>("check_app_update"),
  installAppUpdate: () => invoke<AppSnapshot>("install_app_update"),
  probeDns: () => invoke<AppSnapshot>("probe_dns"),
};

// ---------------------------------------------------------------------------
// 事件
// ---------------------------------------------------------------------------

/** 事件名必须与 `apps/desktop/src/events.rs` 里的常量完全一致。 */
export const EVENTS = {
  runtimeChanged: "runtime://changed",
  coreLog: "core://log",
  latencyUpdated: "nodes://latency",
  probeStarted: "nodes://probe-started",
  nodesChanged: "nodes://changed",
  subscriptionsChanged: "subscriptions://changed",
  settingsChanged: "settings://changed",
  updateProgress: "update://progress",
} as const;

export interface RuntimePayload {
  runtime: CoreRuntime;
  traffic: TrafficSample;
}

export interface LogPayload {
  line: string;
  level: string;
}

export interface ProbeStartedPayload {
  total: number;
}

/** 更新下载进度。`totalBytes` 为 null 表示上游没报，只能显示已下载多少。 */
export interface UpdateProgressPayload {
  label: string;
  done_bytes: number;
  total_bytes: number | null;
}

/** 批量注册监听并在卸载时统一清理。 */
export function subscribe(handlers: {
  onRuntime?: (payload: RuntimePayload) => void;
  onLog?: (payload: LogPayload) => void;
  onLatency?: (results: ProbeResult[]) => void;
  onProbeStarted?: (payload: ProbeStartedPayload) => void;
  onNodesChanged?: () => void;
  onSubscriptionsChanged?: () => void;
  onSettingsChanged?: () => void;
  onUpdateProgress?: (payload: UpdateProgressPayload) => void;
}): () => void {
  const unlisteners: UnlistenFn[] = [];
  let disposed = false;

  const register = async () => {
    const pairs: Array<[string, (e: { payload: unknown }) => void]> = [];
    if (handlers.onRuntime) {
      pairs.push([EVENTS.runtimeChanged, (e) => handlers.onRuntime!(e.payload as RuntimePayload)]);
    }
    if (handlers.onLog) {
      pairs.push([EVENTS.coreLog, (e) => handlers.onLog!(e.payload as LogPayload)]);
    }
    if (handlers.onLatency) {
      pairs.push([EVENTS.latencyUpdated, (e) => handlers.onLatency!(e.payload as ProbeResult[])]);
    }
    if (handlers.onProbeStarted) {
      pairs.push([
        EVENTS.probeStarted,
        (e) => handlers.onProbeStarted!(e.payload as ProbeStartedPayload),
      ]);
    }
    if (handlers.onNodesChanged) {
      pairs.push([EVENTS.nodesChanged, () => handlers.onNodesChanged!()]);
    }
    if (handlers.onSubscriptionsChanged) {
      pairs.push([EVENTS.subscriptionsChanged, () => handlers.onSubscriptionsChanged!()]);
    }
    if (handlers.onSettingsChanged) {
      pairs.push([EVENTS.settingsChanged, () => handlers.onSettingsChanged!()]);
    }
    if (handlers.onUpdateProgress) {
      pairs.push([
        EVENTS.updateProgress,
        (e) => handlers.onUpdateProgress!(e.payload as UpdateProgressPayload),
      ]);
    }

    for (const [name, handler] of pairs) {
      const un = await listen(name, handler);
      // 如果注册过程中已经被卸载，立刻注销，避免泄漏监听器。
      if (disposed) {
        un();
      } else {
        unlisteners.push(un);
      }
    }
  };

  void register();

  return () => {
    disposed = true;
    for (const un of unlisteners) un();
    unlisteners.length = 0;
  };
}

/**
 * 把后端返回的错误统一成可显示的文本。
 *
 * 后端命令的 `Err` 已经是给用户看的中文消息，这里只兜住少数几种
 * 非字符串情况（例如 IPC 序列化失败）。
 */
export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  try {
    return JSON.stringify(e);
  } catch {
    return String(e);
  }
}
