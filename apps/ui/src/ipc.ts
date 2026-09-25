//! Tauri IPC 封装。
//!
//! **所有** `invoke` / `listen` 都必须经过这里，不允许在组件里直接调用。
//! 原因：命令名和事件名是字符串，拼错在 Tauri 里不会报错 —— 只会静静地
//! 拿不到数据。集中在一处至少让「改名」变成一次全局搜索。

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isObject } from "./eventGuards";
import { humanError } from "./failure";
import type { IncidentPreview, IncidentUpload } from "./incident";
import type {
  GlobeData,
  IntentAllowAction,
  IntentAuditRecord,
  IntentExplain,
  IntentSummary,
  MitmStatus,
  RecoveryState,
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
  // 恢复状态在 `runtime.recovery` 里（backend-dev 2026-09-20 定稿）：`runtime`
  // 在**事件与快照两条路**上都整体传输，所以放里面刷新后不会丢；放顶层则会丢。
  // 因此这个接口本身不需要新增字段。
}

/** 恢复状态怎么呈现（纯函数，便于单测「恢复中不得显示为未连接」）。 */
export interface RecoveryView {
  /**
   * 四态：正在恢复 / 上次恢复失败（已退回直连）/
   * **探测已经开始失败、但还没到重建那一步** / 不在恢复流程里。
   */
  phase: "recovering" | "failed" | "degraded" | "idle";
  /** 状态文案（**短**，顶栏徽章用）；`idle` 时为 **null**（不编「未在恢复」）。 */
  text: string | null;
  /**
   * 自救提示（**长句**，仪表盘副文案用；顶栏把它放进 `title`）。
   * **只有 `degraded` 有**，其余状态一律 `null` —— 测试锁着这条。
   */
  hint: string | null;
  /** 按钮语义：connect=可点的「连接」；disconnect=可点的「断开」；recovering=禁用。 */
  button: "connect" | "disconnect" | "recovering";
  /** 恢复刚刚成功（用于「可感知的结束」提示）。 */
  justRecovered: boolean;
}

/**
 * 把（后端给的）恢复状态翻译成界面语义。
 *
 * 六条不变量（测试锁着）：
 * 1. `recovering` 时**按钮绝不是 `connect`** —— 不允许出现「看起来未连接 + 可点的连接按钮」；
 * 2. 文案永远带「恢复」二字，不会退化成「未连接」；
 * 3. **成功之后不留残影**：`last_outcome === "recovered"` 且不在恢复时 phase 回到 `idle`
 *    （只给一次性 `justRecovered`），不会一直显示「正在恢复」；
 * 4. **失败≠断网**：`direct_fallback` 要说清「已退回直连、流量不再走代理」，
 *    并且按钮保持可点（= 手动重连），不留一个无事可做的禁用按钮；
 * 5. 没有任何倒计时：后端没有「计划中的下次重试」（失败即退回直连并退出），
 *    所以**不渲染**倒计时 —— 编一个恒为空的时间字段本身就是编语义。
 * 6. **「可能正在变坏」不等于「已经坏了」**：看门狗是「连续 2 次失败才重建」
 *    （`core.rs` 的 `FAILURES_BEFORE_REBUILD`），所以第 1 次探测失败之后那 10 秒
 *    是**设计内**的窗口。这期间 phase 是 `degraded`：**不改成黄/红**（那会把设计内
 *    过程说成故障），只补一句**可见**的自救提示 `hint`。
 *
 * # 为什么要有 `degraded`（这是它防的故障）
 *
 * 以前这段窗口里界面上**与健康时逐字相同**：绿点、`已连接 · 香港 · 53 ms`、按钮
 * 「断开」（实测于 task-60）。于是用户整机断网时看到的仍然是「一切正常」，他去查
 * 路由器/运营商/节点，**不会想到「先断开」** —— 而 `断开` 会走
 * `supervisor.stop` → helper 回滚系统网络配置（`core.rs:227` 日志「网络配置已回滚」）。
 * 这条提示就是把那个已经存在、但从未被说出来的自救动作说出来。
 */
export function recoveryView(
  recovery: RecoveryState | null | undefined,
  running: boolean,
): RecoveryView {
  const rec = recovery ?? null;
  if (rec?.recovering) {
    return {
      phase: "recovering",
      text: `正在自动恢复（第 ${rec.attempt} 次）`,
      hint: null,
      button: "recovering",
      justRecovered: false,
    };
  }
  // `direct_fallback` **只有在没在跑的时候才成立**。
  //
  // 手动重连成功后 `running === true`，但后端不会重置 `last_outcome`（`core.rs` 对
  // recovery 的写入只有 begin / succeeded / fell_back / set_probe_failures 四处）。
  // 若这里仍然返回 `failed`，界面会在「已经连上、流量正在走代理」时显示红色
  // 「自动恢复失败 / 已退回直连」—— 陈述与事实相反，比漏报更糟。
  if (rec?.last_outcome === "direct_fallback" && !running) {
    return {
      phase: "failed",
      text: `自动恢复失败（第 ${rec.attempt} 次），已退回直连 —— 流量不再走代理`,
      hint: null,
      button: "connect",
      justRecovered: false,
    };
  }
  // ①「探测失败、但看门狗还没开始重建」的那 10–20 秒。
  //
  // 只说事实（已经失败过 N 次），**不猜**下一次会不会重建 —— 阈值在后端。
  // 也**不**把它渲染成失败态：见不变量 6。
  const failures = rec?.probe_failures ?? 0;
  if (running && failures >= 1) {
    return {
      phase: "degraded",
      text: `隧道探测失败 ${failures} 次 · 整机断网时先点「断开」`,
      hint:
        `已连接，但最近一次连通性探测失败（连续 ${failures} 次）。` +
        `如果整台 Mac 都上不了网，先点「断开」再试 —— 断开会还原系统网络配置。`,
      // 连接仍在（按钮仍是「断开」）：状态没变，变的只是「可能正在变坏」。
      button: "disconnect",
      justRecovered: false,
    };
  }
  return {
    phase: "idle",
    text: null,
    hint: null,
    button: running ? "disconnect" : "connect",
    // 「可感知的结束」：后端明确说上次自动重建成功了，且现在确实在跑
    justRecovered: running && rec?.last_outcome === "recovered",
  };
}

/**
 * 从事件载荷里安全取出 `recovery`：事件是**运行时**数据，必须校验而不是信任类型。
 * 畸形/缺席一律当「没有恢复信息」（不抛、不猜、不反推「未在恢复」）。
 */
export function parseRecovery(runtime: unknown): RecoveryState | null {
  if (!isObject(runtime)) return null;
  const v = runtime.recovery;
  if (!isObject(v)) return null;
  const num = (x: unknown): number | null => (typeof x === "number" && Number.isFinite(x) ? x : null);
  const outcome = v.last_outcome === "recovered" || v.last_outcome === "direct_fallback"
    ? v.last_outcome
    : null;
  return {
    recovering: v.recovering === true,
    attempt: num(v.attempt) ?? 0,
    probe_failures: num(v.probe_failures) ?? 0,
    started_unix: num(v.started_unix),
    last_outcome: outcome,
    finished_unix: num(v.finished_unix),
  };
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
