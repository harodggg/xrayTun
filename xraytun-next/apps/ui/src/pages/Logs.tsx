import { LOG_LEVEL_TEXT, formatTime } from '../components/format';
import { useDaemon } from '../store/daemon';

// 只渲染尾部 N 行：日志是无限流，DOM 不能无限增长。
// 这是渲染上限，不是数据截断 —— 页面同时如实说明当前总行数。
const MAX_RENDERED_LOGS = 500;

export function Logs() {
  const { logs } = useDaemon();
  const tail =
    logs.length > MAX_RENDERED_LOGS ? logs.slice(logs.length - MAX_RENDERED_LOGS) : logs;

  return (
    <div className="page logs" data-testid="page-logs">
      <h1 className="page__title">日志</h1>
      <p className="page__hint" data-testid="logs-count">
        来自 daemon 的真实日志行；最多渲染尾部 {MAX_RENDERED_LOGS} 行，当前共 {logs.length} 行。
      </p>

      {tail.length === 0 ? (
        <div className="page__empty" data-testid="logs-empty">
          <p className="page__empty-title">暂无日志</p>
          <p className="page__empty-hint">daemon 尚未推送日志行。</p>
        </div>
      ) : (
        <div className="logs__list">
          {tail.map((line, index) => (
            <div
              className={`log-line log-line--${line.level}`}
              data-testid="log-line"
              data-level={line.level}
              key={`${line.ts_ms}-${index}`}
            >
              <span className="log-line__ts mono">{formatTime(line.ts_ms)}</span>
              {/* 级别必须有文字：颜色只是补充，不能是唯一信息来源。 */}
              <span className="log-line__level">{LOG_LEVEL_TEXT[line.level]}</span>
              <span className="log-line__target mono">{line.target}</span>
              <span className="log-line__message mono">{line.message}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
