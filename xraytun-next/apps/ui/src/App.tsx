import { useEffect, useState } from 'react';
import { ErrorBox } from './components/ErrorBox';
import { Topbar } from './components/Topbar';
import { launchLogLine, launchReportText } from './components/launchReport';
import { toDisplayErrorBody } from './components/format';
import { canReadLaunchReport, daemonLaunchReport } from './shell';
import { Dashboard } from './pages/Dashboard';
import { Logs } from './pages/Logs';
import { Nodes } from './pages/Nodes';
import { Settings } from './pages/Settings';
import { DaemonProvider, useDaemon } from './store/daemon';
import type { DaemonClient } from './transport/client';

// 导航只列真实存在的四个页面。没有拓扑 / 地理这些入口 —— 契约里没有任何连接列表或地理字段，
// 渲染入口就等于承诺了做不到的事。连接方式只有 TUN（daemon 宣告 tun_mode 时启动按钮才可点）。
type PageId = 'dashboard' | 'nodes' | 'logs' | 'settings';

const NAV_ITEMS: { id: PageId; label: string }[] = [
  { id: 'dashboard', label: '仪表盘' },
  { id: 'nodes', label: '节点' },
  { id: 'logs', label: '日志' },
  { id: 'settings', label: '设置' },
];

/**
 * 把外部给的初始页字符串收敛成一个合法页面 id。
 *
 * 这个值来自壳注入的 `window.__XT_INITIAL_PAGE__`（CI / 调试用，见 `main.tsx`），
 * 属于**不可信输入**：不认识的值一律回落到默认页，而不是照单全收 —— 界面不该被一个
 * 环境变量带到不存在的页面上。
 */
function toPageId(raw: string | undefined): PageId {
  const matched = NAV_ITEMS.find((item) => item.id === raw);
  return matched === undefined ? 'dashboard' : matched.id;
}

/**
 * 连不上 daemon 时的「为什么」。
 *
 * 壳在拉起 daemon 那一步会拿到一个结论（没找到二进制 / spawn 被拒 / 报就绪前就退了），
 * 以前它只进 `eprintln`，界面上只剩下一句「连接 socket 失败」——对用户毫无用处。
 * 这里把那句结论原样带出来，并给出 daemon 日志路径。
 *
 * 只在**壳注入了标记**、且**确实拿到了报告**时渲染；取不到就什么都不显示 ——
 * 界面不许替壳编一个「启动失败」。
 */
function TransportDiagnosis() {
  const [report, setReport] = useState<Awaited<ReturnType<typeof daemonLaunchReport>>>(null);

  useEffect(() => {
    if (!canReadLaunchReport()) return undefined;
    let active = true;
    // 只问一次：这是「上次启动的结论」，不是需要盯着看的状态（不轮询、不重试）。
    void daemonLaunchReport().then((next) => {
      if (active) setReport(next);
    });
    return () => {
      active = false;
    };
  }, []);

  const text = launchReportText(report);
  if (text == null) return null;
  const log = launchLogLine(report);
  return (
    <div className="page__hint" data-testid="launch-report">
      <span>{text}</span>
      {log != null && <span className="mono"> · {log}</span>}
    </div>
  );
}

function Shell({ initialPage }: { initialPage: PageId }) {
  const { transportError, reconnect, reconnectPending } = useDaemon();
  const [page, setPage] = useState<PageId>(initialPage);
  const transportFailure = toDisplayErrorBody(transportError);

  return (
    <div className="app">
      <nav className="app__nav" aria-label="主导航">
        {NAV_ITEMS.map((item) => (
          <button
            type="button"
            key={item.id}
            className={`app__nav-item${page === item.id ? ' app__nav-item--active' : ''}`}
            data-testid={`nav-${item.id}`}
            aria-current={page === item.id ? 'page' : undefined}
            onClick={() => setPage(item.id)}
          >
            {item.label}
          </button>
        ))}
      </nav>
      <main className="app__main">
        {/* 顶栏是全局状态条（老版「开始面板」那一块）：只报状态，不放按钮 ——
            启动 / 停止全屏只有仪表板顶部那一处（理由见 components/Topbar.tsx）。 */}
        <Topbar />
        {/* 传输层自己的失败也要原样上报：它和业务失败同样重要，
            静默吞掉会让界面看起来「一切正常」。
            只有这一处额外给一个用户动作：引导链失败后界面停在「断了」，恢复必须由用户
            显式发起（reconnect 用同一个 client 重跑整条链）。文案是「重新连接」，
            因为界面里没有任何东西会自己再连一次。 */}
        {transportFailure != null && (
          <ErrorBox
            error={transportFailure}
            testId="transport-error"
            actionLabel="重新连接"
            actionTestId="transport-reconnect"
            actionPending={reconnectPending}
            onUserAction={reconnect}
          />
        )}
        {/* 只有「连不上（io）」才可能和壳启动 daemon 那一步有关；别把这件事挂在
            任何其它错误上，否则用户会顺着它查一条无关的线索。 */}
        {transportFailure != null && transportFailure.code === 'io' && <TransportDiagnosis />}
        {page === 'dashboard' && <Dashboard onNavigate={setPage} />}
        {page === 'nodes' && <Nodes />}
        {page === 'logs' && <Logs />}
        {page === 'settings' && <Settings />}
      </main>
    </div>
  );
}

export function App({ client, initialPage }: { client?: DaemonClient; initialPage?: string } = {}) {
  // ux 的验收测试自己包 DaemonProvider 并直接渲染 <App/>；
  // main.tsx 则把按环境选好的真实 client 传进来。两种接线都必须成立，
  // 所以只在拿到 client 时才由 App 自己建 Provider。
  //
  // `initialPage` 只影响**初始**落在哪一页，之后仍由界面自己的导航状态说了算。
  const page = toPageId(initialPage);
  if (client == null) {
    return <Shell initialPage={page} />;
  }
  return (
    <DaemonProvider client={client}>
      <Shell initialPage={page} />
    </DaemonProvider>
  );
}
