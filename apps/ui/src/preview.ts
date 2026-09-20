/**
 * 浏览器预览桥接（**只用于开发**，绝不进生产路径）。
 *
 * # 为什么需要它
 *
 * 界面完全依赖 Tauri 的 `invoke`：在普通浏览器里打开 `vite dev` 只会看到
 * 一连串 "window.__TAURI_INTERNALS__ is undefined"，什么都渲染不出来。
 * 于是每改一次样式都要重新编译整个 Rust 外壳（几分钟），而样式迭代本该是秒级的。
 *
 * 这里用一份**合成的快照**顶替后端：页面、布局、样式、交互都能照常渲染，
 * 只有真正读写系统的那几个动作是假的（返回快照本身）。
 *
 * # 怎么用
 *
 * ```bash
 * cd apps/ui && npm run dev
 * # 浏览器打开 http://localhost:5173/?preview=1
 * ```
 *
 * 安全性：`preview.ts` 只在 **dev 构建**且 URL 带 `?preview=1` 时才会被动态加载；
 * 生产打包（`npm run build`）里 `import.meta.env.DEV` 恒为 false，这段代码不会执行，
 * 也不会被打进产物（动态 import 的分支被静态求值后摇掉）。
 *
 * 快照的**字段名**由 `apps/desktop/tests/type_contract.rs` 保证与 Rust 一致；
 * 这里的取值只需"看起来像真的"，不必与任何真实状态对应。
 *
 * # 本文件已被拆分（task-15）
 *
 * 1288 行时它同时装着假快照、假拓扑、假连接、假日志与自检探针 —— 改一处要读全篇。
 * 现在按职责拆成（都在 `src/` 下，前缀 `preview*`）：
 *
 * * `previewSnapshot.ts`   —— `snapshot` 的假数据 + `?state=` 场景
 * * `previewTopology.ts`   —— `routing_topology` + `?traffic=`
 * * `previewConnections.ts` —— `recent_connections` + `?connections=`
 * * `previewLogs.ts`       —— `tail_logs` 的假日志
 * * `previewProbe.ts`      —— `window.__topologyProbe()` 自检探针（只读 DOM）
 *
 * 本文件只剩一件事：**装/卸桥接**（`installPreviewBridge`）。
 */


import { scenarioSnapshot } from "./previewSnapshot";
import { topologyScenario } from "./previewTopology";
import { connectionsScenario } from "./previewConnections";
import { MOCK_LOGS } from "./previewLogs";
import { installTopologyProbe } from "./previewProbe";

// 对外接口保持不变：`main.tsx` 只取 `installPreviewBridge`；
// 其余原先从这里导出的名字**原样再导出**，免得任何消费者被迫改路径。
export { MOCK_LOGS, installTopologyProbe, scenarioSnapshot };
export type {
  TopologyProbeFrameGaps,
  TopologyProbeGuideToVisible,
  TopologyProbeResult,
  TopologyProbeSteps,
  TopologyProbeTruck,
  TopologyProbeVerdict,
} from "./previewProbe";

/** 安装桥接。返回 uninstall，便于热更新时清理。 */
export function installPreviewBridge(): () => void {
  // 拓扑自检探针（`window.__topologyProbe()`）：只读 DOM，与假后端无关，
  // 但只在预览模式下需要，所以挂在这里一起装、一起卸。
  const uninstallProbe = installTopologyProbe();

  // **按事件名分组**记录监听者。
  //
  // 早先是一个扁平的 `Map<id, cb>`，派发时不看事件名 —— 于是推一条日志会把
  // 「延迟结果」的处理器也一起叫醒，它对着日志载荷做 `for...of` 立刻抛
  // `results is not iterable`，React 整棵树卸载（窗口与标签页一起消失）。
  // 真实 Tauri 的事件是按名字派发的，这里必须一样。
  //
  // 关联过程：`listen()` 先调 `transformCallback(cb)` 拿到 id，紧接着
  // `invoke('plugin:event|listen', { event, handler: id })`。所以在
  // `transformCallback` 时记下「刚拿到的 id」，在随后的 listen 调用里
  // 把事件名补上即可（两者在同一个同步段内发生）。
  const listeners = new Map<string, Map<number, (payload: unknown) => void>>();
  let nextId = 1;
  let pendingHandlerId: number | null = null;
  /** 所有登记过的回调（按 id），`listen` 时按事件名归入 `listeners`。 */
  const allCallbacks = new Map<number, (payload: unknown) => void>();

  const internals = {
    invoke: async (cmd: string, _args?: unknown): Promise<unknown> => {
      // 事件订阅相关命令：按事件名登记/注销监听者。
      if (cmd.startsWith("plugin:event|")) {
        const a = (_args ?? {}) as { event?: string; eventId?: number };
        if (cmd === "plugin:event|listen") {
          if (a.event && pendingHandlerId !== null) {
            const bucket = listeners.get(a.event) ?? new Map();
            const cb = allCallbacks.get(pendingHandlerId);
            if (cb) bucket.set(pendingHandlerId, cb);
            listeners.set(a.event, bucket);
            pendingHandlerId = null;
          }
          return nextId++;
        }
        if (cmd === "plugin:event|unlisten") {
          if (a.event && a.eventId !== undefined) {
            listeners.get(a.event)?.delete(a.eventId);
          }
          return null;
        }
        return null;
      }
      switch (cmd) {
        case "snapshot":
          return scenarioSnapshot();
        case "routing_topology":
          return topologyScenario();
        case "globe_data":
          // 用与 Rust 侧一致的形状；坐标取自实测（本机=大理，节点=香港）
          return {
            route: {
              from: { ip: "39.144.146.165", country: "中国", city: "广州市", lat: 23.1317, lon: 113.266, isp: "China Mobile", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 25.61", "ip-api.com: 25.69"] },
              to: { ip: "45.207.197.185", country: "香港", city: "香港", lat: 22.3193, lon: 114.169, isp: "Vapeline Technology", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 22.28", "ip-api.com: 22.32"] },
              bytes: 9_846_000_000,
              node_name: "Xray-45.207.197.185",
            },
            origin: { ip: "39.144.146.165", country: "中国", city: "广州市", lat: 23.1317, lon: 113.266, isp: "China Mobile", source: "ipwho.is", consistent: true, sources: ["ipwho.is: 25.61", "ip-api.com: 25.69"] },
            error: null,
          };
        case "explain_dest": {
          // 判定用真实规则会命中哪条 —— 预览里给一个**与真实配置同形**的结果，
          // 便于核对界面文案；真实判定由 Rust 侧完成（已与真实核心对拍）。
          const dest = String((_args as { dest?: string })?.dest ?? "");
          const isCn = /(baidu|qq|taobao|cn$|\.cn$)/i.test(dest);
          const isGoogle = /google|gmail|youtube/i.test(dest);
          const isAds = /doubleclick|ads?\./i.test(dest);
          const tag = isAds ? "preset-ads" : isGoogle ? "preset-proxy-google" : isCn ? "preset-cn-domain" : "internal-fallback";
          const out = isAds ? "block" : isGoogle || !isCn ? "node-n1d232c6b8c7a5004" : "direct";
          return {
            rule_index: 3,
            rule_tag: tag,
            outbound: out,
            reasons: [isAds ? "命中 geosite:category-ads-all" : isGoogle ? "命中 geosite:google" : isCn ? "命中 geosite:cn" : "网络 tcp 匹配"],
            undecidable: [],
          };
        }
        case "tail_logs":
          return MOCK_LOGS;
        case "recent_connections":
          return connectionsScenario();
        case "diagnostics":
          return "XrayTun 0.8.0（预览数据）\nmacOS 26.6.2\n核心 26.9.9\nhelper: 未安装";
        default:
          // 其余命令一律返回当前快照：按钮有反馈、状态不跳动。
          return scenarioSnapshot();
      }
    },
    transformCallback: (cb: (payload: unknown) => void) => {
      const id = nextId++;
      allCallbacks.set(id, cb);
      pendingHandlerId = id; // 紧接着的 listen 调用会用它认领事件名
      return id;
    },
    unregisterCallback: (id: number) => {
      allCallbacks.delete(id);
      for (const bucket of listeners.values()) bucket.delete(id);
    },
    convertFileSrc: (p: string) => p,
  };

  (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = internals;
  // `@tauri-apps/api/event` 的 `_unlisten` 不走 `invoke`，而是**直接**调
  // `window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener(event, eventId)`。
  // 真实 Tauri 由 webview 注入这个全局；预览里以前没有它，于是每次
  // register/unlisten 都抛 `Cannot read properties of undefined` —— 那是
  // **预览桥接的缺口，不是产品缺陷**（生产路径用的是 Tauri 自己的 internals）。
  // 这里补上按事件名注销的方法，与 `plugin:event|unlisten` 命令保持同一语义。
  (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__ = {
    unregisterListener: (event: string, eventId: number) => {
      listeners.get(event)?.delete(eventId);
      allCallbacks.delete(eventId);
    },
  };
  // 页面里的 Tauri API 版本探测会读这个
  (window as unknown as Record<string, unknown>).__TAURI__ = {};

  // 开发用：把一条核心日志推给已经订阅的 handler。
  //
  // 真实的 Tauri 事件由 Rust 侧 `emit`，浏览器里没有那条通路；而「跟随滚动」
  // 这类行为只有**真的来新日志**才能验证（往 DOM 里插元素不会触发 React 的
  // 状态更新）。所以这里按 `core://log` 的载荷形状直接调用监听者。
  (window as unknown as Record<string, unknown>).__emitCoreLog = (line: string, level = "info") => {
    // 只发给订阅了 `core://log` 的那些 —— 与真实事件派发一致。
    const bucket = listeners.get("core://log");
    if (!bucket) return;
    for (const cb of bucket.values()) {
      cb({ event: "core://log", payload: { line, level } });
    }
  };

  // 开发用：推一次 `runtime://changed`（task-22 的自动恢复要验证「界面看得见自愈」）。
  //
  // 为什么必须有它：恢复状态**只在事件里**出现（后端把它放进 `runtime_changed`
  // 载荷），光刷新页面复现不出来 —— 那正是原缺陷的一部分（前端只在少数事件时拉快照，
  // 于是看不到看门狗正在自愈）。
  (window as unknown as Record<string, unknown>).__emitRuntimeChanged = (
    patch: { runtime?: Record<string, unknown>; traffic?: Record<string, unknown> } = {},
  ) => {
    const bucket = listeners.get("runtime://changed");
    if (!bucket) return false;
    const snap = scenarioSnapshot();
    const payload = {
      runtime: { ...(snap.runtime as unknown as Record<string, unknown>), ...(patch.runtime ?? {}) },
      traffic: { ...(snap.traffic as unknown as Record<string, unknown>), ...(patch.traffic ?? {}) },
    };
    for (const cb of bucket.values()) cb({ event: "runtime://changed", payload });
    return true;
  };

  /**
   * 三种自愈状态的一键复现（给 lead / 设计同学复现用，省得每次手写载荷）。
   *
   * ```js
   * __recoveryDemo("recovering", 2)  // 正在第 2 次重建
   * __recoveryDemo("recovered", 2)   // 第 2 次重建成功（可感知的结束）
   * __recoveryDemo("failed", 3)      // 第 3 次失败 → 退回直连
   * __recoveryDemo("idle")           // 回到普通状态
   * ```
   */
  (window as unknown as Record<string, unknown>).__recoveryDemo = (
    phase: "recovering" | "recovered" | "failed" | "idle",
    attempt = 1,
  ) => {
    const nowS = Math.floor(Date.now() / 1000);
    const emit = (window as unknown as Record<string, unknown>).__emitRuntimeChanged as (
      p: { runtime?: Record<string, unknown> },
    ) => boolean;
    if (phase === "recovering") {
      return emit({
        runtime: {
          running: false,
          recovery: {
            recovering: true,
            attempt,
            probe_failures: 2,
            started_unix: nowS,
            last_outcome: null,
            finished_unix: null,
          },
        },
      });
    }
    if (phase === "recovered") {
      return emit({
        runtime: {
          running: true,
          recovery: {
            recovering: false,
            attempt,
            probe_failures: 0,
            started_unix: nowS - 4,
            last_outcome: "recovered",
            finished_unix: nowS,
          },
        },
      });
    }
    if (phase === "failed") {
      return emit({
        runtime: {
          running: false,
          recovery: {
            recovering: false,
            attempt,
            probe_failures: 3,
            started_unix: nowS - 4,
            last_outcome: "direct_fallback",
            finished_unix: nowS,
          },
        },
      });
    }
    return emit({ runtime: { running: true, recovery: null } });
  };

  // `?recovery=recovering|recovered|failed` —— 挂载后自动推一次，方便截图与回归
  // （不必每次手写 CDP 注入）。等监听者注册好再推，最多等 5 秒。
  const wantRecovery = new URLSearchParams(location.search).get("recovery");
  if (wantRecovery === "recovering" || wantRecovery === "recovered" || wantRecovery === "failed") {
    let tries = 0;
    const push = () => {
      const fn = (window as unknown as Record<string, unknown>).__recoveryDemo as
        | ((p: string, a?: number) => boolean)
        | undefined;
      const ok = fn?.(wantRecovery, 2);
      if (ok !== true && tries++ < 25) window.setTimeout(push, 200);
    };
    window.setTimeout(push, 200);
  }

  return () => {
    uninstallProbe();
    delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__TAURI_EVENT_PLUGIN_INTERNALS__;
    delete (window as unknown as Record<string, unknown>).__emitCoreLog;
    delete (window as unknown as Record<string, unknown>).__emitRuntimeChanged;
    delete (window as unknown as Record<string, unknown>).__recoveryDemo;
    listeners.clear();
    allCallbacks.clear();
  };
}
