import type { ConnectPhase, Stage } from '../transport/contract';
import { PHASE_TEXT, STAGE_TEXT } from './format';

// 状态徽章：只由 ConnectionView.stage 决定颜色与文案。
// phase 子元素只在 connecting 且 phase 非空时出现；没有 phase 就不渲染，
// 因为「阶段未知」也不该被一个空标签伪装成已知。
export function StatusBadge({
  stage,
  phase,
}: {
  stage: Stage | null;
  phase?: ConnectPhase | null;
}) {
  if (stage == null) {
    // 连状态快照都还没到时不能假设「未连接」，只能如实显示未知。
    return (
      <span className="badge badge--unknown" data-testid="stage-badge">
        未知
      </span>
    );
  }
  return (
    <span className={`badge badge--${stage}`} data-testid="stage-badge">
      <span className="badge__label">{STAGE_TEXT[stage]}</span>
      {stage === 'connecting' && phase != null && (
        <span
          className={`badge__phase badge__phase--${phase}`}
          data-testid="phase-label"
        >
          {PHASE_TEXT[phase]}
        </span>
      )}
      {stage === 'connecting' && phase != null && (
        <span className="badge__code mono" data-testid="phase-code">
          {phase}
        </span>
      )}
    </span>
  );
}
