import { useState } from "react";
import { api } from "./ipc";
import { StoreProvider, useStore } from "./store";
import { MODE_LABEL, formatBytes, formatRate, type ProxyMode } from "./types";
import Dashboard from "./pages/Dashboard";
import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import Routing from "./pages/Routing";
import Topology from "./pages/Topology";
import Logs from "./pages/Logs";
import Settings from "./pages/Settings";

type View = "dashboard" | "nodes" | "subscriptions" | "routing" | "topology" | "logs" | "settings";

const NAV: Array<{ id: View; label: string }> = [
  { id: "dashboard", label: "仪表盘" },
  { id: "nodes", label: "节点" },
  { id: "subscriptions", label: "订阅" },
  { id: "routing", label: "规则" },
  { id: "topology", label: "拓扑" },
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
  const { snapshot, error, clearError } = useStore();

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
          {view === "dashboard" && <Dashboard onNavigate={(v) => setView(v as View)} />}
          {view === "nodes" && <Nodes />}
          {view === "subscriptions" && <Subscriptions />}
          {view === "routing" && <Routing />}
        {view === "topology" && <Topology />}
          {view === "logs" && <Logs />}
          {view === "settings" && <Settings />}
        </div>
      </main>
    </div>
  );
}

function TopBar({ view }: { view: View }) {
  const { snapshot, busy, run } = useStore();
  const [pending, setPending] = useState<ProxyMode | null>(null);

  const title = NAV.find((n) => n.id === view)?.label ?? "";
  const mode = snapshot?.settings.mode ?? "system_proxy";
  const running = snapshot?.runtime.running ?? false;
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
    <header className="topbar">
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

      <span className={`dot${running ? " dot--on" : ""}`} />
      <button
        className={`btn ${running ? "" : "btn--primary"}`}
        disabled={runBusy || mode === "direct"}
        onClick={() => void toggleRun()}
        title={mode === "direct" ? "直连模式下无需启动核心" : ""}
      >
        {runBusy ? <span className="spin" /> : null}
        {running ? "断开" : "连接"}
      </button>
    </header>
  );
}
