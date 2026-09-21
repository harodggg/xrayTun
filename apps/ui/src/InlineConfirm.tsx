/**
 * 内联二次确认（破坏性操作用）。
 *
 * # 为什么需要它
 *
 * 节点删除、订阅删除、日志「清空」以前都是**点一下就执行**（全仓库 `window.confirm`
 * 0 命中）。其中两处后果还不小：删订阅会**连带删掉该订阅带来的全部节点**
 * （`commands/nodes.rs:197`），清空日志会**删除日志文件本身**（`store.rs:213`）。
 *
 * # 为什么是内联确认，不是 `window.confirm`
 *
 * 1. **能就地说明后果与对象**：系统 confirm 只有一个字符串，而且要写清「删哪个节点、
 *    它是不是订阅带来的、删了会怎样」时那句话会很长、还把按钮挤到系统默认位置；
 * 2. **不阻塞**：`window.confirm` 会冻结渲染与事件循环，而这里要在同一屏、
 *    紧挨被操作对象的位置给出确认；
 * 3. **样式与语义可控**：深色界面里弹一块系统灰框是断裂的；内联确认用站内的
 *    `btn--danger`，并且是普通的可聚焦按钮、`role="group"` 带 `aria-label`，
 *    键盘与读屏都可用；
 * 4. Tauri 的 WebView 里 `window.confirm` 的行为随平台而异（部分环境直接返回 false），
 *    把它当成安全网本身就是不可靠的。
 *
 * # 为什么不是「同一个按钮点两次」
 *
 * 「再点一次」的两次点击**语义完全相同** —— 误触、手抖、连点都会直接通过，
 * 等于把确认退化成一道仪式。这里第二次点击的目标是**另一颗按钮**（「确认删除」/
 * 「确认清空」），并且确认态里**必须先显示后果**（问句里写明删什么、能否恢复），
 * 所以它强制用户读一句、再点另一处。
 *
 * # 为什么是确认而不是撤销（日志「清空」）
 *
 * 已核实：`clear_logs` 会删除**活动文件与备份**（`crates/xt-core/src/store.rs:213`
 * 的 `clear_logs` + 测试 `clear_logs_removes_files_too`），且没有「追加日志」的 IPC
 * 可以把内容写回去 —— 也就是**服务端不留副本、前端也无从恢复**。
 * 因此这里给确认而不是撤销；如果将来后端保留一份可恢复的副本（例如清空前先重命名），
 * 就该换成「已清空 · 撤销」而不是确认。
 */
import { useEffect, useState, type MouseEvent as ReactMouseEvent } from "react";

interface Props {
  /** 未确认时那颗按钮上的文字。 */
  label: string;
  /** 确认态里的问句：必须写明**后果**与**对象**。 */
  question: string;
  /** 确认按钮文字（动词明确，不要写「是」）。 */
  confirmLabel: string;
  /** 未确认按钮的 class（破坏性操作应带 `btn--danger`）。 */
  className?: string;
  /** 与其它按钮一致的禁用态（例如操作进行中）。 */
  disabled?: boolean;
  /** 悬停说明（沿用原来的 title）。 */
  title?: string;
  onConfirm: () => void;
}

export function InlineConfirm({
  label,
  question,
  confirmLabel,
  className,
  disabled,
  title,
  onConfirm,
}: Props) {
  const [armed, setArmed] = useState(false);

  // Esc 取消：确认态是个临时状态，必须有一条明确的退出路径。
  useEffect(() => {
    if (!armed) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setArmed(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [armed]);

  // 所有点击都不冒泡：这些按钮常常长在「点击整行就切换节点」的行里
  // （`Nodes.tsx` 的行容器带 onClick），冒泡会把切换也一起触发。
  const stop = (e: ReactMouseEvent) => e.stopPropagation();

  if (!armed) {
    return (
      <button
        type="button"
        className={className}
        disabled={disabled}
        title={title}
        onClick={(e) => {
          stop(e);
          setArmed(true);
        }}
      >
        {label}
      </button>
    );
  }

  return (
    <span className="confirm" role="group" aria-label={question} onClick={stop}>
      <span className="confirm__question">{question}</span>
      <button
        type="button"
        className="btn btn--danger"
        disabled={disabled}
        onClick={(e) => {
          stop(e);
          setArmed(false); // 无论成败都收起确认态，避免停留在「已确认」的悬空状态
          onConfirm();
        }}
      >
        {confirmLabel}
      </button>
      <button type="button" className="btn btn--ghost" onClick={(e) => {
        stop(e);
        setArmed(false);
      }}>
        取消
      </button>
    </span>
  );
}
