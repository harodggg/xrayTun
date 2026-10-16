// 连不上 daemon 时，把**壳自己那一步**的结论翻译成一句人话。
//
// 为什么单独一个纯模块：这句话只有在「连接失败」时才出现，而失败在浏览器里很难造；
// 写成纯函数后每条分支都能单测（与 `startButton.ts` / `notices.ts` 同一套做法）。
//
// 红线：**只翻译壳真正报过的结论**。`null`（没报告）→ 一个字都不显示，
// 绝不写「daemon 启动失败」这种壳没说过的话。

import type { LaunchReport } from '../shell';

/** 返回值是给人看的一句话；`null` = 没有可说的（界面就不渲染这一段）。 */
export function launchReportText(report: LaunchReport | null): string | null {
  if (report == null) return null;
  const reason = report.reason == null || report.reason.trim() === '' ? null : report.reason;
  switch (report.outcome) {
    case 'unavailable':
      return `壳没能启动 daemon：${reason ?? '（壳没有给出原因）'}`;
    case 'not_listening':
      // 「没能确认在监听」≠「启动失败」：这是把握上的差别，措辞不许含糊。
      return `壳拉起了 daemon 进程（pid ${report.pid ?? '未知'}），但没能确认它在监听：${
        reason ?? '（壳没有给出原因）'
      }`;
    case 'spawned':
      return `壳已确认 daemon 在监听（pid ${report.pid ?? '未知'}）`;
    case 'already_listening':
      return '壳启动前就已有 daemon 在监听';
    default:
      // 壳将来加了新结论：如实原样带出来，绝不猜它的意思。
      return `壳报告：${String(report.outcome)}`;
  }
}

/**
 * 「去哪看一手现场」。日志路径来自壳；壳没给就返回 `null`（不编默认路径 ——
 * 环境变量可以把日志指到别处，编一个就等于把用户引到错的地方）。
 */
export function launchLogLine(report: LaunchReport | null): string | null {
  if (report == null || report.log_path == null || report.log_path.trim() === '') return null;
  return `daemon 日志：${report.log_path}`;
}
