import { useEffect, useState } from 'react';

import { launchLogLine } from './launchReport';
import { canReadLaunchReport, daemonLaunchReport } from '../shell';

/**
 * 失败时告诉用户「一手现场在哪」。
 *
 * 日志路径来自壳（`daemon_launch_report` 的 `log_path`），**壳没给就什么都不显示** ——
 * 环境变量可以把日志指到别处，编一个默认路径等于把用户引到错的地方。
 *
 * 只问一次：这是「壳启动时算出来的路径」，不是需要盯着看的状态（不轮询、不重试）。
 */
export function DaemonLogHint({ testId }: { testId: string }) {
  const [line, setLine] = useState<string | null>(null);

  useEffect(() => {
    if (!canReadLaunchReport()) return undefined;
    let active = true;
    void daemonLaunchReport().then((report) => {
      if (active) setLine(launchLogLine(report));
    });
    return () => {
      active = false;
    };
  }, []);

  if (line == null) return null;
  return (
    <div className="page__hint mono" data-testid={testId}>
      {line}
    </div>
  );
}
