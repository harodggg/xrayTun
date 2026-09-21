import { useState } from "react";
import { api, recoveryView, type RecoveryView } from "./ipc";
import { StoreProvider, useStore } from "./store";
import { MODE_LABEL, formatBytes, formatRate, type ProxyMode } from "./types";
import Dashboard from "./pages/Dashboard";
import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import Routing from "./pages/Routing";
import Globe from "./pages/Globe";
import Topology from "./pages/Topology";
import Logs from "./pages/Logs";
import Settings from "./pages/Settings";

type View = "dashboard" | "nodes" | "subscriptions" | "routing" | "topology" | "globe" | "logs" | "settings";

/* ======================================================================
 * 顶栏状态线的语义（task-45）
 *
 * 那条 2px 的线横跨整窗、常驻可见，是应用里最「环境化」的信号。它必须回答
 * **一个问题**：*此刻这台机器的流量被代理覆盖到什么程度？*
 *
 * 它以前是一句 **假陈述**：`styles.css` 把 `.topbar` 的底边写死成 `--ok`（绿），
 * 于是直连、系统代理、未连接、恢复中、已退回直连 **全都是绿的**。
 * 而在这套配色里绿色 = 已受保护 —— 直连模式下用户会以为受保护，实际毫无代理；
 * 自动兜底退回直连时更严重：**流量已经在裸奔，界面还在报平安**。
 *
 * 判据全部来自后端真实字段，**不允许前端按时间或次数猜**：
 *   `settings.mode` / `runtime.running` / `runtime.routes_committed` /
 *   `runtime.last_error` / `runtime.recovery`（经 `recoveryView` 翻译成三段）。
 *
 * 五种色调，与 `styles.css` 的 `--status-*` 一一对应：
 *
 * | tone      | 含义                             | 颜色                    |
 * |-----------|----------------------------------|-------------------------|
 * | `on`      | 整机受保护（TUN 在跑）           | `--ok` 绿               |
 * | `partial` | 部分覆盖（系统代理在跑）         | `--accent` 蓝           |
 * | `off`     | **没有代理覆盖**（直连/未运行）  | `--status-off` 中性灰   |
 * | `busy`    | 正在自愈 / 隧道建好但路由没接管  | `--warn` 琥珀           |
 * | `failed`  | **代理承诺已破**（退回直连/报错）| `--danger` 红           |
 *
 * 两条硬性约束：
 * 1. **直连与系统代理绝不能是绿色** —— 绿色专属于「整机受保护」（TUN）。
 * 2. **默认值是 `off`（中性灰），不是绿。** 漏配状态、快照还没到时不该
 *    「默认受保护」。
 *
 * 状态优先级刻意与 `pages/Dashboard.tsx:68-88` 的 `state` 派生**保持一致**
 * （那里是状态词的既有唯一真源）：恢复态排在「未连接」之前，
 * `routes_committed === false` 必须降级 —— 否则「隧道建好了但默认路由还没接管」
 * 会被画成绿色，又是同一族假陈述。
 */
export type TopbarTone = "on" | "partial" | "off" | "busy" | "failed";

/** tone → 顶栏修饰类名。组件与测试共用，避免两处各拼一遍字符串。 */
export const TOPBAR_TONE_CLASS: Record<TopbarTone, string> = {
  on: "topbar--on",
  partial: "topbar--partial",
  off: "topbar--off",
  busy: "topbar--busy",
  failed: "topbar--failed",
};

/**
 * tone → 小圆点的修饰类名。
 *
 * 底边那条线和这个点**必须同色**：它们相距几像素、说的是同一件事。
 * 改这一条之前请先看 `topbarStatus` 的说明 —— 两处不一致比一处错更难查。
 */
export const DOT_TONE_CLASS: Record<TopbarTone, string> = {
  on: "dot--on",
  partial: "dot--partial",
  off: "dot--off",
  busy: "dot--warn",
  failed: "dot--failed",
};

export interface TopbarStatus {
  tone: TopbarTone;
  /** 这个状态的一句话说明（`title` + 屏幕阅读器共用）。每个分支都有，不编造。 */
  detail: string;
}

export function topbarStatus(input: {
  mode: ProxyMode;
  running: boolean;
  routesCommitted: boolean;
  lastError: string | null;
  recoveryPhase: RecoveryView["phase"];
}): TopbarStatus {
  const { mode, running, routesCommitted, lastError, recoveryPhase } = input;

  // 1) 故障最优先。`direct_fallback` 时 `mode` **仍然是 `"tun"`** ——
  //    若按模式取色就会画成绿色，而那正是这条缺陷最严重的一种表现。
  if (recoveryPhase === "failed") {
    return { tone: "failed", detail: "自动恢复失败，已退回直连 —— 流量不再走代理" };
  }
  // 2) 自愈中：此刻既不是「受保护」也不是「未连接」。
  if (recoveryPhase === "recovering") {
    return { tone: "busy", detail: "正在自动恢复 —— 看门狗在重建隧道，不需要手动连接" };
  }
  // 3) 直连是用户**主动选的**模式，中性报告即可（不是警告，也不是正常）。
  if (mode === "direct") {
    return { tone: "off", detail: "直连模式 —— 不接管任何流量" };
  }
  // 4) 核心没在跑，且上次是失败的 → 如实说故障。
  if (lastError !== null && !running) {
    return { tone: "failed", detail: `核心未运行：${lastError}` };
  }
  // 5) 核心没在跑（用户还没点连接）→ 空闲，中性。
  if (!running) {
    return { tone: "off", detail: "未连接 —— 核心没有运行" };
  }
  // 6) 关键：**进程在跑 ≠ 流量走了代理**。TUN 的承诺是「接管全部流量」，
  //    而两阶段启动里隧道会先建好、默认路由还没接管 —— `CoreRuntime` 的字段注释
  //    （`state.rs`）明确要求 UI 表达这个中间态，此时画绿色就是假陈述。
  //    **只对 TUN 生效**：`routes_committed` 是 TUN 的闸门
  //    （`supervisor.rs` 里提交路由那段在 `if mode == Tun` 分支内），
  //    系统代理模式不接管路由，这个字段对它没有意义 —— 拿它压系统代理会
  //    把「部分覆盖正常工作中」误报成「流量还没走代理」。
  if (mode === "tun") {
    if (!routesCommitted) {
      return { tone: "busy", detail: "隧道已建立，默认路由尚未接管 —— 流量还没有走代理" };
    }
    // 7) 只有这里才是名副其实的绿色。
    return { tone: "on", detail: "TUN 模式运行中 —— 整机流量受保护" };
  }
  // 8) 系统代理：只有读系统代理的应用走代理 → 部分覆盖，用强调色而不是绿。
  return { tone: "partial", detail: "系统代理运行中 —— 只有读取系统代理的应用走代理" };
}

const NAV: Array<{ id: View; label: string }> = [
  { id: "dashboard", label: "仪表盘" },
  { id: "nodes", label: "节点" },
  { id: "subscriptions", label: "订阅" },
  { id: "routing", label: "规则" },
  { id: "topology", label: "拓扑" },
  { id: "globe", label: "地球仪" },
  { id: "logs", label: "日志" },
  { id: "settings", label: "设置" },
];

export default function App() {
  return (
    <StoreProvider>
      <Shell initialView={initialViewFromUrl()} />
    </StoreProvider>
  );
}

/**
 * 只给浏览器预览用的初始页面：`?view=nodes`。
 *
 * 生产环境恒为 undefined（`import.meta.env.DEV` 为 false），正式版总是从
 * 仪表盘开始 —— 这个参数只是让截图工具与手工预览能直接落在某一页，
 * 不必逐个点导航。
 */
function initialViewFromUrl(): View | undefined {
  if (!import.meta.env.DEV) return undefined;
  const v = new URLSearchParams(location.search).get("view");
  return NAV.some((n) => n.id === v) ? (v as View) : undefined;
}

function Shell({ initialView }: { initialView?: View }) {
  const [view, setView] = useState<View>(initialView ?? "dashboard");
  const { snapshot, error, clearError, recoveredAttempt, dismissRecovered } = useStore();

  const nodeCount = snapshot?.nodes.length ?? 0;
  const subCount = snapshot?.subscriptions.length ?? 0;

  const badgeFor = (id: View): string | null => {
    if (id === "nodes") return nodeCount ? String(nodeCount) : null;
    if (id === "subscriptions") return subCount ? String(subCount) : null;
    return null;
  };

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="sidebar__brand">
          Xray<span>Tun</span>
        </div>
        <nav className="sidebar__nav">
          {NAV.map((item) => (
            <button
              key={item.id}
              className={`nav-item${view === item.id ? " is-active" : ""}`}
              onClick={() => setView(item.id)}
            >
              <span>{item.label}</span>
              {badgeFor(item.id) && <span className="nav-item__badge">{badgeFor(item.id)}</span>}
            </button>
          ))}
        </nav>
        {/* 版本信息放在这里，一眼能看到「客户端 + 核心」两个版本。
            分开列是必要的：升级客户端不等于升级核心，而两者都会影响行为
            （核心版本决定支不支持原生 TUN）。 */}
        <div
          className="sidebar__footer"
          title={
            snapshot
              ? `客户端 ${snapshot.app_version}\n核心 ${snapshot.core.version ?? "未找到"}\n${
                  snapshot.core.path ?? ""
                }`
              : ""
          }
        >
          {snapshot ? (
            <>
              <div>客户端 v{snapshot.app_version}</div>
              <div className="sidebar__footer-sub">
                核心 {snapshot.core.version ?? (snapshot.core.error ? "未找到" : "检测中…")}
              </div>
            </>
          ) : (
            "正在加载…"
          )}
        </div>
      </aside>

      <main className="main">
        <TopBar view={view} />
        <div className="content">
          {error && (
            <div className="banner banner--error">
              <span>⚠︎</span>
              <div style={{ flex: 1 }}>{error}</div>
              <button className="btn btn--ghost" onClick={clearError}>
                关闭
              </button>
            </div>
          )}
          {/* 「可感知的结束」：自动恢复成功后不能悄悄变回「已连接」——
              给一次明确的完成提示，8 秒后自己消失（也可手动关掉）。
              只在**恢复真的发生过**时出现（后端 `last_outcome === "recovered"`）。 */}
          {recoveredAttempt !== null && (
            <div className="banner banner--info">
              <span>✓</span>
              <div style={{ flex: 1 }}>
                已自动恢复连接（第 {recoveredAttempt} 次自动重建成功）—— 隧道已重建，无需手动操作。
              </div>
              <button className="btn btn--ghost" onClick={dismissRecovered}>
                知道了
              </button>
            </div>
          )}
          {view === "dashboard" && <Dashboard onNavigate={(v) => setView(v as View)} />}
          {view === "nodes" && <Nodes />}
          {view === "subscriptions" && <Subscriptions />}
          {view === "routing" && <Routing />}
        {view === "topology" && <Topology />}
        {view === "globe" && <Globe />}
          {view === "logs" && <Logs />}
          {view === "settings" && <Settings />}
        </div>
      </main>
    </div>
  );
}

export function TopBar({ view }: { view: View }) {
  const { snapshot, busy, run, recovery } = useStore();
  const [pending, setPending] = useState<ProxyMode | null>(null);

  const title = NAV.find((n) => n.id === view)?.label ?? "";
  const mode = snapshot?.settings.mode ?? "system_proxy";
  const running = snapshot?.runtime.running ?? false;
  /**
   * 自动恢复（task-22）：看门狗在自愈时，顶栏**不能**看起来像「没连接、快来点」——
   * 点了就是和看门狗抢（`core.rs` 注释提过启动会被多处并发调用）。
   * 三态由 `recoveryView` 这个纯函数决定（有单测锁着「恢复中不得显示为未连接」）。
   */
  const rv = recoveryView(recovery, running);
  /**
   * 顶栏状态线的色调（task-45）。**两条线共用同一个 tone** —— 底边那条 2px 的线
   * 和右边那个小圆点如果在同一块区域里给出两种结论，比一条错的更糟。
   *
   * `routesCommitted` 在快照缺席时取 `false`（=「还没接管」）而不是 `true`：
   * 未知状态应当降级成「不可信」，不能默认「已受保护」。
   */
  const status = topbarStatus({
    mode,
    running,
    routesCommitted: snapshot?.runtime.routes_committed ?? false,
    lastError: snapshot?.runtime.last_error ?? null,
    recoveryPhase: rv.phase,
  });
  const traffic = snapshot?.traffic;
  // 窗口用的是 `hiddenTitle`（见 tauri.conf.json），macOS 的标题栏文字是
  // 隐藏的 —— 这条顶栏才是用户真正看到的「标题栏」。所以网速要显示在这里，
  // 而不是只调 window.set_title。
  const showSpeed = (snapshot?.settings.show_speed_in_title ?? true) && running;

  const switchMode = async (next: ProxyMode) => {
    if (next === mode) return;
    setPending(next);
    await run("mode", () => api.setMode(next));
    setPending(null);
  };

  const toggleRun = async () => {
    if (running) {
      await run("stop", () => api.stop());
    } else {
      await run("start", () => api.start());
    }
  };

  const modeBusy = busy === "mode" || pending !== null;
  const runBusy = busy === "start" || busy === "stop";

  return (
    /*
     * 状态线的颜色由 `topbar--<tone>` 决定（见 `styles.css` 的 `--status-*`）。
     * `title` 与下面那个 `sr-only` 的 live region 让**不依赖颜色**也能读到状态：
     * 那条线本身没有文字，色盲用户/灰度截图里五种色调是读不出来的。
     */
    <header className={`topbar ${TOPBAR_TONE_CLASS[status.tone]}`} title={status.detail}>
      <span className="sr-only" role="status">
        {status.detail}
      </span>
      <div className="topbar__title">{title}</div>

      {showSpeed && traffic ? (
        <div
          className="topbar__speed"
          title={`本次连接累计：下载 ${formatBytes(traffic.rx_bytes)}，上传 ${formatBytes(traffic.tx_bytes)}`}
        >
          <span className="topbar__speed-item">
            <span className="topbar__speed-arrow">↓</span>
            {formatRate(traffic.rx_rate)}
          </span>
          <span className="topbar__speed-item">
            <span className="topbar__speed-arrow">↑</span>
            {formatRate(traffic.tx_rate)}
          </span>
        </div>
      ) : null}

      <div className="segmented" role="group" aria-label="代理模式">
        {(["direct", "system_proxy", "tun"] as ProxyMode[]).map((m) => (
          <button
            key={m}
            className={mode === m ? "is-active" : ""}
            disabled={modeBusy}
            onClick={() => void switchMode(m)}
            title={
              m === "tun"
                ? "通过 utun 虚拟网卡接管全部流量（需要已安装 helper）"
                : m === "system_proxy"
                  ? "只设置系统 HTTP/SOCKS 代理"
                  : "不接管任何流量"
            }
          >
            {MODE_LABEL[m]}
          </button>
        ))}
      </div>

      {/* 自动恢复中的状态：必须是**可读的一句话**，而不是一个沉默的灰点 */}
      {rv.phase === "recovering" && (
        <span className="badge badge--ok" title="看门狗正在自动重建隧道，不需要手动点「连接」">
          {rv.text}
        </span>
      )}
      {rv.phase === "failed" && (
        <span className="badge badge--unknown" title={rv.text ?? undefined}>
          自动恢复失败
        </span>
      )}

      <span className={`dot ${DOT_TONE_CLASS[status.tone]}`} />
      <button
        className={`btn ${rv.button === "connect" ? "btn--primary" : ""}`}
        disabled={runBusy || mode === "direct" || rv.button === "recovering"}
        onClick={() => void toggleRun()}
        title={
          mode === "direct"
            ? "直连模式下无需启动核心"
            : rv.button === "recovering"
              ? "正在自动恢复 —— 现在点「连接」会打断看门狗的重建，所以先禁用；恢复会自动完成"
              : rv.phase === "failed"
                ? "自动恢复失败，已退回直连；点这里可手动重连"
                : ""
        }
      >
        {runBusy ? <span className="spin" /> : null}
        {rv.button === "recovering" ? "正在恢复…" : running ? "断开" : "连接"}
      </button>
    </header>
  );
}
