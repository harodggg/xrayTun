/**
 * 快照读不到时的**页面占位**（task-23 A1）。
 *
 * # 它防的是「空态冒充错误态」
 *
 * 以前五个页面各自写 `if (!snapshot) return <div className="empty">正在加载…</div>;`，
 * 而节点/订阅页更糟：`snapshot?.nodes ?? []` 之后直接渲染「还没有任何节点 /
 * 还没有订阅」。于是**一次 IPC 故障**会变成：
 *
 * * 仪表盘/规则/设置页 —— 「正在加载…」永远不停（用户一直等）；
 * * 节点/订阅页 —— **给错原因**：告诉用户"你没有节点"，把他引向"去添加订阅"，
 *   而真相是数据没读回来（同一个「查不到 ≠ 没有」的坑，这次在最显眼的两个列表页）。
 *
 * # 三态口径（与 `store.tsx` 的 `logsLoad` 同一套）
 *
 * | phase | 显示 |
 * |---|---|
 * | `loading` | 「正在读取状态…」（**不是**「正在加载…」那种没有主语的等待） |
 * | `failed`  | 后端原文 + 「重试」（调既有 `refresh()`）+ 明确说清这不是「没有数据」 |
 * | `loaded`  | 本组件不出现 —— 页面渲染真实内容（空数组时才允许出现空态文案） |
 */
import { useStore } from "./store";

export default function SnapshotFallback() {
  const { snapshotPhase, error, refresh } = useStore();

  if (snapshotPhase === "failed") {
    return (
      <div className="page">
        <div className="banner banner--error" role="alert">
          <span>⚠︎</span>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div>读不到状态：{error ?? "后端没有给出原因"}</div>
            {/* 这句是这一屏**唯一**要说清的事：不是你没数据，是数据没读回来。 */}
            <div className="banner__steps">
              这一页现在没有任何数据可显示 —— 这不等于「你还没有节点/订阅」，是状态没读回来。
            </div>
          </div>
          <button className="btn" onClick={() => void refresh()}>
            重试
          </button>
        </div>
      </div>
    );
  }

  return <div className="empty">正在读取状态…</div>;
}
