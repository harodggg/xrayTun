// 仪表板的提示横幅：把「需要用户知道的事」按严重度收集起来。
//
// 为什么单独一个纯模块：老版把这套排序逻辑埋在组件里，只能渲染整个仪表盘才能断言；
// 抽出来之后每条横幅的 rank / 文案 / 动作都能单测（老版也为此把 collectNotices 导出过）。
//
// 排序约定（rank 越小越急）与老版一致：先「现在就是坏的」，再「需要处理」，最后「仅供参考」。
//
// 有界：notice 缓冲最多留 100 条，本函数只取**最近一条 error** 与**最近一条 warning/info**
// （各最多一条）。理由：横幅是「现在要你看的事」，不是历史列表；把 100 条旧 notice 全铺出来
// 只会让最新的那条被淹掉。历史仍然完整留在 store 里，没有任何东西被丢掉。

import type { Notice } from '../transport/contract';

/** 横幅右侧动作能跳到哪一页。与 App.tsx 的 PageId 对齐，不引入第二套导航词汇。 */
export type BannerTarget = 'nodes' | 'logs' | 'settings';

export interface Banner {
  key: string;
  tone: 'error' | 'warn' | 'info';
  rank: number;
  text: string;
  action: { label: string; target: BannerTarget } | null;
}

export function collectBanners(notices: Notice[]): Banner[] {
  const out: Banner[] = [];

  // `connection.last_error` **不在这里**出横幅：仪表板用 ErrorBox 原样渲染它（含 code），
  // 那已经是同一条失败的完整陈述。再加一条横幅就是同屏两处说同一件事 ——
  // 老版 task-23 D1 修过的正是这个重复。

  // rank 1 / 2：daemon 发来的真实 notice，两条互不遮蔽（最近一条 error、最近一条非 error）。
  const lastError = [...notices].reverse().find((notice) => notice.severity === 'error');
  const lastOther = [...notices].reverse().find((notice) => notice.severity !== 'error');
  if (lastError != null) {
    out.push({
      key: `notice-error-${lastError.at_ms}`,
      tone: 'error',
      rank: 1,
      text: lastError.message,
      action: null,
    });
  }
  if (lastOther != null) {
    out.push({
      key: `notice-${lastOther.severity}-${lastOther.at_ms}`,
      tone: lastOther.severity === 'warning' ? 'warn' : 'info',
      rank: 2,
      text: lastOther.message,
      action: null,
    });
  }

  // 空节点（rank 3）**不在这里出横幅**：那种情况下大启动按钮本身就是「导入节点」，
  // 按钮下方还写着「节点目录为空 —— 先在「节点」页导入订阅原文或分享链接」。同一个动作同屏两处
  // 正是老版 task-72 收敛掉的东西，所以这里不再复述。

  return out.sort((a, b) => a.rank - b.rank);
}
