import { createRoot } from 'react-dom/client';
import { App } from './App';
import './styles.css';
import { createTauriClient } from './transport/tauri';
import { createUnixSocketClient } from './transport/unixSocket';

// 两种运行环境共用同一个 DaemonClient 契约，界面本身不感知差异：
// - Tauri WebView（生产）：走 tauri.ts，由 Rust 侧代理 AF_UNIX。
// - 浏览器 / 活体验收：走 unixSocket.ts，直接连 AF_UNIX。
declare global {
  interface Window {
    __TAURI__?: unknown;
    __XT_SOCKET__?: string;
    /**
     * 初始页（可选）。由壳在页面脚本之前注入，来源是环境变量 `XT_INITIAL_PAGE`
     * （白名单见 `apps/desktop/src/lib.rs`）。用途是 CI / 调试时直接打开某一页截图 ——
     * CI 上点不动界面（AppleScript 够不到 WebView 里的按钮），没有它就只能靠人手截。
     * 取值不合法时 `App` 会回落到默认页。
     */
    __XT_INITIAL_PAGE__?: string;
  }
}

const DEFAULT_SOCKET_PATH = '/run/xraytun/daemon.sock';

// 不引入 vite/client 类型依赖：直接对 import.meta 做结构化读取，
// 这样 tsconfig 里有没有 vite types 都能编译。
const env = (import.meta as ImportMeta & { env?: Record<string, string | undefined> }).env;
const socketPath = window.__XT_SOCKET__ ?? env?.VITE_XT_SOCKET ?? DEFAULT_SOCKET_PATH;

const container = document.getElementById('root');
if (container == null) {
  // 挂载点缺失是配置错误，必须大声失败而不是渲染到 body 里假装正常。
  throw new Error('index.html 缺少 #root 挂载点');
}

const client =
  window.__TAURI__ != null ? createTauriClient(socketPath) : createUnixSocketClient(socketPath);

createRoot(container).render(
  <App client={client} initialPage={window.__XT_INITIAL_PAGE__} />,
);
