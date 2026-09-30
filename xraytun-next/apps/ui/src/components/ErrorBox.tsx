import type { DisplayError } from './format';

// 失败展示：code 与 message 原样输出。
//
// 为什么不做「连接失败，请重试」这类改写：本项目没有重试（契约里没有 Retry），
// 任何改写都会让用户以为还有一条自动的路。原始 message 是唯一可追溯的真相。
export function ErrorBox({ error, testId }: { error: DisplayError; testId?: string }) {
  return (
    <div className="error-box" data-testid={testId}>
      {error.code != null && <span className="error-box__code">{error.code}</span>}
      <span className="error-box__message">{error.message}</span>
    </div>
  );
}
