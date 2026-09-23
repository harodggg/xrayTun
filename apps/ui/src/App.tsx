import { useCallback, useEffect, useState } from "react";
import { api, recoveryView } from "./ipc";
// 状态语义的**唯一真源**（task-45 建、task-47 合并）：顶栏的线/点与仪表盘的状态区
// 都从 `appStatus` 取，任何地方再抄一份判断都是把同一族「假陈述」种回去。
import {
  appStatus,
  DOT_TONE_CLASS,
  runButtonDisabled,
  systemProxyBadge,
  TOPBAR_TONE_CLASS,
} from "./topbarStatus";
import { StoreProvider, useStore } from "./store";
import { MODE_LABEL, formatBytes, formatRate, type ProxyMode } from "./types";
import Dashboard from "./pages/Dashboard";
import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import Routing from "./pages/Routing";
import Globe from "./pages/Globe";
import Topology from "./pages/Topology";
import Logs from "./pages/Logs";
import Settings, { categoryOfSection } from "./pages/Settings";

type View = "dashboard" | "nodes" | "subscriptions" | "routing" | "topology" | "globe" | "logs" | "settings";

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
  if (NAV.some((n) => n.id === v)) return v as View;
  // 分节深链（如 `#set-helper`）：直接落在设置页的对应分类，不必先进仪表盘。
  // 这一段**不受预览开关限制** —— 它是正式版的 URL 契约，不是调试参数。
  if (initialSectionFromHash()) return "settings";
  return undefined;
}

/**
 * 从 URL 锚点取出设置页分节 id：`#set-helper` → `"set-helper"`。
 *
 * 只有确实属于设置页的分节才认（用设置页自己的分类表判断）；其他页面的锚点
 * 或未知 id 一律返回 null，免得把任意 `#foo` 都当成「要进设置页」。
 */
function initialSectionFromHash(): string | null {
  const id = location.hash.replace(/^#/, "");
  return id && categoryOfSection(id) ? id : null;
}

function Shell({ initialView }: { initialView?: View }) {
  const [view, setView] = useState<View>(initialView ?? "dashboard");
  // 要跳到的设置分节。只有「从别处带着目标进设置」时才非空（深链、或状态卡片
  // 上的按钮）；用户自己点侧栏进设置时是 null，设置页就按记忆/默认分类走。
  const [settingsTarget, setSettingsTarget] = useState<string | null>(() => initialSectionFromHash());
  const { snapshot, error, clearError, recoveredAttempt, dismissRecovered } = useStore();

  // 入参用 string 而不是 View：Dashboard 的 onNavigate 契约就是 `(view: string)`，
  // 这里收窄一次即可；等它加上可选的 target 参数（状态卡片直接指到某个设置分节）
  // 也不用再改这里。
  const onNavigate = useCallback((next: string, target?: string) => {
    setView(next as View);
    setSettingsTarget(target ?? null);
  }, []);

  // 离开设置页就把目标丢掉：否则下次进来会莫名其妙跳到上一回那个分节。
  useEffect(() => {
    if (view !== "settings") setSettingsTarget(null);
  }, [view]);

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
          {view === "dashboard" && <Dashboard onNavigate={onNavigate} />}
          {view === "nodes" && <Nodes />}
          {view === "subscriptions" && <Subscriptions />}
          {view === "routing" && <Routing />}
        {view === "topology" && <Topology />}
        {view === "globe" && <Globe />}
          {view === "logs" && <Logs />}
          {view === "settings" && <Settings focusSection={settingsTarget} />}
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
   * 状态语义（task-45；task-47 起与仪表盘状态区**同一个真源**）。
   * 顶栏底边那条 2px 的线和右边那个小圆点都用这个 tone —— 两处相距几像素、
   * 说的是同一件事，给出两种结论比一处错更糟。
   *
   * 快照缺席时的降级值是刻意的：`running ?? false`、`routesCommitted ?? false`、
   * `corePath ?? null` —— 未知状态一律落到「不可信」，**不能默认「已受保护」**。
   */
  const status = appStatus({
    mode,
    running,
    routesCommitted: snapshot?.runtime.routes_committed ?? false,
    lastError: snapshot?.runtime.last_error ?? null,
    corePath: snapshot?.core.path ?? null,
    recovery: rv,
    // 端口只用于「系统代理」模式的文案；快照缺席时给 null（那就一个数字都不写）。
    socksPort: snapshot?.settings.socks_port ?? null,
    httpPort: snapshot?.settings.http_port ?? null,
  });
  const socksPort = snapshot?.settings.socks_port ?? null;
  /** 只有在「核心运行中 + 系统代理模式」时才有内容（task-72，见 `systemProxyBadge`）。 */
  const proxyBadge = systemProxyBadge(mode, running, socksPort);
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
    <header
      className={`topbar ${TOPBAR_TONE_CLASS[status.tone]}`}
      // task-68：自救提示（`rv.hint`）现在由 `appStatus` 折进 `detail` —— 同一份
      // 文本既进 `title`（悬停可读），也进下面那个 `role="status"` 的 live region
      // （**读屏用户必须听到它**，否则那句自救指令只有看得见的人才收得到）。
      title={status.detail}
    >
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
                  ? // task-120：**这句原来是「只设置系统 HTTP/SOCKS 代理」，而实现从来
                    // 没有设置过系统代理**（全仓 `setwebproxy`/`scutil`/`SCDynamicStore`
                    // 0 命中，只在 `model.rs:451` 的注释里写着这个意图）。
                    // 同一屏的状态区说的却是「系统代理未被本应用修改：需要手动指向…」——
                    // 两句话互相矛盾，而这条 tooltip 是在**承诺产品做不到的事**。
                    // 现在只说**已经成立**的那半：本应用只开本地入口、不改系统设置。
                    "只开本地 SOCKS/HTTP 入口（127.0.0.1）；本应用不修改系统代理设置，需要你手动把浏览器或系统代理指向它"
                  : "不接管任何流量"
            }
          >
            {MODE_LABEL[m]}
          </button>
        ))}
      </div>

      {/* 切换进度：以前这里只有一个 `disabled` —— 那几秒到几十秒里界面完全沉默，
          用户看到的就是「点了没反应」。（task-54） */}
      {modeBusy && (
        <span
          className="badge badge--unknown"
          role="status"
          title="切换模式需要重启核心：先停掉再按新模式起，请等它完成"
        >
          {running ? "正在切换模式（重启核心，可能十几秒）…" : "正在切换模式…"}
        </span>
      )}

      {/* 行为变更提示：**模式只是一个偏好**。未连接时点它不再隐式连接核心
          （以前会，代价是一次完整连接）。这条提示由状态直接推导，不是一次性
          flag —— 所以不会留下过期的「点连接开始」。

          `rv.phase === "idle"` 是必须的：自动恢复中与已退回直连时，界面刚刚
          告诉用户「不要点连接 / 已退回直连」，这条「点右侧「连接」开始」会
          当场把话反过来说（实测：恢复中曾与「正在自动恢复（第 2 次）」同屏）。 */}
      {!running && mode !== "direct" && !modeBusy && rv.phase === "idle" && (
        <span className="badge badge--unknown" title="模式已保存；真正开始连接的是「连接」按钮">
          已选「{MODE_LABEL[mode]}」，点右侧「连接」开始
        </span>
      )}

      {/* 「系统代理」模式的真相（task-68）：**本应用从不修改系统代理设置** ——
          它只提供本地入站入口（`docs/07-roadmap-and-risks.md` 里「系统代理模式
          真正生效」仍是未勾选项，全仓对系统代理的写入 0 命中）。

          task-72：**只在核心运行中 + 系统代理模式**显示。`SystemProxy` 是 `#[default]`，
          新用户一打开就是它，那时没有端口需要指向 —— 显示它只是噪音；连上之后
          它才是真事实，也正是用户需要它的时刻。仪表盘 `sub` 与 live region 里的
          同一句话**不受这个条件影响**（载体按条件出现，信息不消失）。 */}
      {proxyBadge && (
        <span className="badge badge--unknown" title={status.detail}>
          {proxyBadge}
        </span>
      )}

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
      {/* 「可能正在变坏」（task-60）：第 1 次探测失败之后、看门狗重建之前的窗口。
          刻意用**中性**徽章而不是黄色/红色 —— 看门狗本来就是「连续 2 次才重建」，
          这一段是**设计内**的过程，染成告警色等于把正常过程说成故障（那是另一种
          假陈述）。真正要传达的是「你有一个已知有效的自救动作」，见 `hint`。 */}
      {rv.phase === "degraded" && (
        <span className="badge badge--unknown" title={rv.hint ?? undefined}>
          {rv.text}
        </span>
      )}

      <span className={`dot ${DOT_TONE_CLASS[status.tone]}`} />
      <button
        className={`btn ${rv.button === "connect" ? "btn--primary" : ""}`}
        disabled={runButtonDisabled(rv, { runBusy, mode })}
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
