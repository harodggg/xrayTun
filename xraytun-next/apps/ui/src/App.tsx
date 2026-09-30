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

function Shell() {
  const { transportError } = useDaemon();
  const [page, setPage] = useState<PageId>('dashboard');
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
            静默吞掉会让界面看起来「一切正常」。 */}
        {transportFailure != null && (
          <ErrorBox error={transportFailure} testId="transport-error" />
        )}
        {page === 'dashboard' && <Dashboard />}
        {page === 'nodes' && <Nodes />}
        {page === 'logs' && <Logs />}
        {page === 'settings' && <Settings />}
      </main>
    </div>
  );
}

export function App({ client }: { client?: DaemonClient } = {}) {
  // ux 的验收测试自己包 DaemonProvider 并直接渲染 <App/>；
  // main.tsx 则把按环境选好的真实 client 传进来。两种接线都必须成立，
  // 所以只在拿到 client 时才由 App 自己建 Provider。
  if (client == null) {
    return <Shell />;
  }
  return (
    <DaemonProvider client={client}>
      <Shell />
    </DaemonProvider>
  );
}
