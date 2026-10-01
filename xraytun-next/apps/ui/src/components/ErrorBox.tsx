import type { DisplayError } from './format';

// 失败展示：code 与 message 原样输出。
//
// 为什么不做「连接失败，请重试」这类改写：本项目没有重试（契约里没有「失败后自动再来一次」），
// 任何改写都会让用户以为还有一条自动的路。原始 message 是唯一可追溯的真相。
//
// 可选的用户动作（actionLabel / onUserAction）：它是**用户自己**发起的一次新尝试，不是界面在
// 自动恢复 —— 所以按钮文案必须写成「重新连接」这种手动动作的措辞。不传回调时这个盒子和原来
// 完全一样（只显示 code 与 message），业务失败的那些调用点不受影响。
export function ErrorBox({
  error,
  testId,
  actionLabel = '重新连接',
  actionPending = false,
  actionTestId,
  onUserAction,
}: {
  error: DisplayError;
  testId?: string;
  /** 按钮文案；只在传了 onUserAction 时才会用到。 */
  actionLabel?: string;
  /** 为真时按钮不可点：反映「这一按已经在跑」，避免连点重复发起同一件事。 */
  actionPending?: boolean;
  /** 新按钮的 data-testid：调用方按自己的 testid 惯例给。 */
  actionTestId?: string;
  onUserAction?: () => void;
}) {
  return (
    <div className="error-box" data-testid={testId}>
      {error.code != null && <span className="error-box__code">{error.code}</span>}
      <span className="error-box__message">{error.message}</span>
      {onUserAction != null && (
        <button
          type="button"
          className="btn error-box__action"
          data-testid={actionTestId}
          disabled={actionPending}
          onClick={() => onUserAction()}
        >
          {actionLabel}
        </button>
      )}
    </div>
  );
}
