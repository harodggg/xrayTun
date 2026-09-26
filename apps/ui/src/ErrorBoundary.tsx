//! 顶层错误边界：把未捕获的渲染异常变成**可见的**错误面板。
//!
//! 没有它时 React 会卸载整棵树 ⇒ WebView 里只剩黑屏（见 `fatalError.tsx` 的说明）。

import React from "react";
import { FatalPanel, copyToClipboard, formatError, type FatalInfo } from "./fatalError";

interface State {
  info: FatalInfo | null;
}

export class ErrorBoundary extends React.Component<{ children: React.ReactNode }, State> {
  state: State = { info: null };

  static getDerivedStateFromError(e: unknown): State {
    return { info: formatError(e) };
  }

  componentDidCatch(e: unknown): void {
    copyToClipboard(formatError(e));
  }

  render(): React.ReactNode {
    if (this.state.info) return <FatalPanel info={this.state.info} />;
    return this.props.children;
  }
}
