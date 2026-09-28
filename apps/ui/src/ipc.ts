//! Tauri IPC 封装。
//!
//! **所有** `invoke` / `listen` 都必须经过这里，不允许在组件里直接调用。
//! 原因：命令名和事件名是字符串，拼错在 Tauri 里不会报错 —— 只会静静地
//! 拿不到数据。集中在一处至少让「改名」变成一次全局搜索。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { humanError } from "./failure";
import type { IncidentPreview, IncidentUpload } from "./incident";
import type {
  AuditSyncPreview,
  AuditSyncRevoke,
  AuditSyncRun,
  AuditSyncStatus,
  GlobeData,
  IntentAllowAction,
  IntentAuditRecord,
  IntentExplain,
  IntentSummary,
  MitmStatus,
  ObserveReportFile,
  NodeExport,
  RecentConnections,
  RouteExplanation,
  Topology,
  AppSnapshot,
  AppSettings,
  CoreRuntime,
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

  // ---- 「报告问题」（task-131，契约由 Lead 冻结，后端实现是 task-130）---------
  //
  // ⚠️ `incident_anomalies(limit)` **故意没有封装**：卡里只给了 `Anomaly[]`，
  // 没给字段。猜一个形状就是编接口（已报 Lead）。角标只用下面这个计数。
  /** 只在本地打包并返回清单，**不上传**。 */
  incidentPreview: () => invoke<IncidentPreview>("incident_preview"),
  incidentUpload: (bundlePath: string) =>
    invoke<IncidentUpload>("incident_upload", { bundlePath }),
  /** 本地待上报的异常条数（被动哨兵）。 */
  incidentAnomalyCount: () => invoke<number>("incident_anomaly_count"),

  // ---- 意图过滤（Jev 判定）-----------------------------------------------
  //
  // `intent_apply` 是**唯一**会把规则推给核心的动作，它会重连一次 ——
  // 界面必须让用户明确点它，不能自动调（规则在核心启动时才下发）。
  intentStatus: () => invoke<IntentSummary>("intent_status"),
  intentAudit: (limit?: number) => invoke<IntentAuditRecord[]>("intent_audit", { limit }),
  intentExplain: (host: string) =>
    invoke<IntentExplain | null>("intent_explain", { host }),
  /** 放行一个被误杀的域名。动作由用户选（`direct` / `proxy`），我们不替他猜。 */
  intentAllow: (host: string, action: IntentAllowAction) =>
    invoke<IntentSummary>("intent_allow", { host, action }),
  intentClearCache: () => invoke<number>("intent_clear_cache"),
  intentApply: () => invoke<IntentSummary>("intent_apply"),

  // ---- 审计自动同步（契约由 Lead 冻结：`docs/design/AUDIT-SYNC.md` §5–§8）----
  //
  // 与「意图过滤」同一族数据，但**状态在另一条命令上**：这几条只读写本机的同步
  // 状态与一次可选的上传，不经过 `settings.json`（所以不走 `save_settings`）。
  //
  // ⚠️ **返回字段**是 snake_case 且与 Rust 逐字一致；而 `invoke` 的**入参名**
  // 沿用本仓库既有写法（camelCase，Tauri 宏默认转 snake_case）—— 与
  // `incidentUpload` 传 `bundlePath` 对应 Rust 的 `bundle_path` 是同一条约定。
  /** 读本机同步状态。**不联网**（不构造任何请求）。 */
  auditSyncStatus: () => invoke<AuditSyncStatus>("audit_sync_status"),
  /** 开/关自动同步。关闭时后端**零网络请求**；已上传的数据不因此被删。 */
  auditSyncSetEnabled: (enabled: boolean) =>
    invoke<AuditSyncStatus>("audit_sync_set_enabled", { enabled }),
  /** 改上传目标；传**空串 = 恢复默认**（`https://xraytun.top`）。 */
  auditSyncSetBaseUrl: (baseUrl: string) =>
    invoke<AuditSyncStatus>("audit_sync_set_base_url", { baseUrl }),
  /** 设置上传 token；传**空串 = 清除**。写入 Keychain，读回时不回显。 */
  auditSyncSetToken: (token: string) =>
    invoke<AuditSyncStatus>("audit_sync_set_token", { token }),
  /** 立即同步一次（只传 `day < 今天(UTC)` 的完整天）。会真的联网。 */
  auditSyncNow: () => invoke<AuditSyncRun>("audit_sync_now"),
  /**
   * 预览将要上传的内容（选一天；不传 = 后端挑一天）。
   * **只读本机数据，不联网** —— 明文 bundle 不会离开这台机器。
   */
  auditSyncPreview: (day?: string | null) =>
    invoke<AuditSyncPreview>("audit_sync_preview", { day: day ?? null }),
  /** 撤回全部已上传（服务端删除）；`deleted` 是**服务端确认**的对象数。 */
  auditSyncRevoke: () => invoke<AuditSyncRevoke>("audit_sync_revoke"),

  // ---- MITM（可选的内容级判定）------------------------------------------
  //
  // **装根证书与起代理是两件事**：装证书会改系统钥匙串（唯一会改系统状态的动作），
  // 起代理只在本机监听。而引导规则要等**核心重连**才生效 —— 状态里的
  // `core_restart_required` 就是那一步的判据。
  mitmStatus: () => invoke<MitmStatus>("mitm_status"),
  /** 把本会话的根证书装进系统钥匙串（经特权 helper；失败会返回可读原因）。 */
  mitmInstallCa: () => invoke<MitmStatus>("mitm_ca_install"),
  /** 从系统钥匙串撤掉本会话的根证书（幂等），并停掉代理。 */
  mitmRemoveCa: () => invoke<MitmStatus>("mitm_ca_remove"),
  /** 按当前设置起/停本地 MITM 代理。 */
  mitmApply: () => invoke<MitmStatus>("mitm_apply"),
  /**
   * 把观察结论导出到用户给的**绝对路径**；返回写好的路径。
   * 与数据目录留档同源；失败会返回可读原因（相对路径 / 没有摘要 / 写不进去）。
   */
  mitmObserveExport: (path: string) =>
    invoke<string>("mitm_observe_export", { path }),
  /** 清空本会话观察结论并删除数据目录留档文件；返回最新状态。 */
  mitmObserveClear: () => invoke<MitmStatus>("mitm_observe_clear"),
  /**
   * 读回上一轮留在数据目录里的观察结论（重启后仍能看到"看到了什么"）。
   * `null` = 还没留过档；文件坏掉会 reject（不许静默当成没有）。
   */
  mitmObserveSaved: () => invoke<ObserveReportFile | null>("mitm_observe_saved"),

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

  /**
   * 位置数据：本机与出口节点的地理位置（多源互校：ipwho.is / ip-api.com / ipapi.co）。
   *
   * `force=false`（默认）时后端按**公网 IP** 复用缓存 —— IP 没变就不重新联网查询；
   * `force=true` 是「重新定位」按钮：忽略缓存重查一次。
   */
  globeData: (force = false) => invoke<GlobeData>("globe_data", { force }),

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
  /**
   * 自动版本检测跑完了一次（task-188：启动 20 s 后查一次 + 每 6 h 复查）。
   *
   * ⚠️ **载荷刻意不读**：它带的是 `latest_app` / `checked_at` / `check_error`，**没有**
   * `app_update_available` —— 那个字段由后端按当前版本号在**每个快照**里重算
   * （`commands/snapshot.rs::update_status_with`，注释写明「不让前端自己比版本」）。
   * 而「有没有新版」只该由它决定 ⇒ 本事件的正确用法是**重新拉快照**（见 `store.tsx`），
   * 否则就得在前端再实现一遍版本比较，那就是第二处真源。
   *
   * 名字必须与 `apps/desktop/src/events.rs::APP_UPDATE_CHECKED` 一致：Tauri 里事件名
   * 拼错**不报错**，只会静静地收不到 —— 那正是「自动检测了却像什么都没发生」的成因。
   */
  appUpdateChecked: "app://update-checked",
} as const;

export interface RuntimePayload {
  runtime: CoreRuntime;
  traffic: TrafficSample;
  // `runtime` 在**事件与快照两条路**上都整体传输，所以后端新增的运行时字段
  // 刷新后不会丢；放顶层则会丢。因此这个接口本身不需要跟着新增字段。
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
  /**
   * 自动版本检测完成（`app://update-checked`）。**没有载荷参数**：它唯一的用法是
   * 「重新拉一次快照」，载荷不带 `app_update_available` 且前端不允许自己比版本
   * （见 `EVENTS.appUpdateChecked` 的注释）。
   */
  onAppUpdateChecked?: () => void;
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
    if (handlers.onAppUpdateChecked) {
      // **不 cast 载荷**：我们不读它（见 `EVENTS.appUpdateChecked`），
      // 少一处「把未知数据当已知形状用」的机会。
      pairs.push([EVENTS.appUpdateChecked, () => handlers.onAppUpdateChecked!()]);
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
 * 把后端返回的错误统一成**人能读**的文本。
 *
 * 后端命令的 `Err` 已经是给用户看的中文消息（`Result<_, String>`），
 * 但 `invoke` 的 reject 载荷**不保证**是字符串（序列化失败、桥接层包装、
 * 命令不存在…）。旧实现最后两条兜底是
 * `JSON.stringify(e)` → `"{}"` 与 `catch { String(e) }` → `"[object Object]"`，
 * 也就是用户会看到一句既不是原因也不是办法的乱码 —— 而且它看起来**像**一个
 * 结论，用户没法判断「界面是不是根本没拿到错误」。
 *
 * 现在把这件事交给 `failure.ts::humanError`：能读出人话就读，读不出来就
 * **明说读不出来**（`null` / `{}` / 循环引用都有各自的说法）。
 * 「下一步动作」在 `failure.ts::nextSteps`，两者都不解析事件 notice。
 */
export function errorText(e: unknown): string {
  return humanError(e);
}
