import { useState } from 'react';
import { ErrorBox } from './components/ErrorBox';
import { toDisplayErrorBody } from './components/format';
import { Dashboard } from './pages/Dashboard';
import { Logs } from './pages/Logs';
import { Nodes } from './pages/Nodes';
import { Settings } from './pages/Settings';
import { DaemonProvider, useDaemon } from './store/daemon';
import type { DaemonClient } from './transport/client';

// 导航只列本轮真实存在的四个页面。没有 TUN / 拓扑 / 地理这些入口，
// 因为契约里没有对应能力，渲染入口就等于承诺了做不到的事。
type PageId = 'dashboard' | 'nodes' | 'logs' | 'settings';

const NAV_ITEMS: { id: PageId; label: string }[] = [
  { id: 'dashboard', label: '状态' },
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
        {page === 'dashboard' && <Dashboard />}
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
