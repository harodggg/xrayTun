/**
 * 审计自动同步（`audit_sync_*`）：**一份数据源 + 一组纯函数**。
 *
 * # 为什么单独一个文件
 *
 * 同一个状态要在**两处**显示：设置页那张卡片（开关/地址/token/状态区/两个动作）与
 * 意图页审计区的那一行。两处各写一遍请求逻辑，就会出现「一边刷新、另一边不动」，
 * 以及两份措辞各说各话 —— 所以请求只在这里发（`useAuditSyncStatus`），
 * 两处都从它读。
 *
 * # 两条硬规矩（都写进测试）
 *
 * 1. **没有证据就不许说成功**：`last_ok_unix === null` 只能说「从未成功上传过」，
 *    不许出现「正常 / 成功」这类结论（见 `docs/design/AUDIT-SYNC.md` §8）。
 * 2. **关闭就是关闭**：开关关着时界面只陈述「关闭」，且**不给任何点击诱饵**
 *    （不摆一个点了没反应的「立即同步」）—— 关闭时后端本来就不构造任何请求。
 */
import { useCallback, useEffect, useState } from "react";

import { api, errorText } from "./ipc";
import type { AuditSyncRevoke, AuditSyncRun, AuditSyncStatus } from "./types";

/**
 * 上传目标的默认值，与 Rust 侧一致（`docs/design/AUDIT-SYNC.md` §4 的 Base）。
 *
 * 它只用于**界面占位与校验**：真正落盘的值以后端返回的 `status.base_url` 为准。
 */
export const DEFAULT_AUDIT_BASE_URL = "https://xraytun.top";

/**
 * 「上传地址能不能存」的**唯一判据**（返回人话原因；`null` = 可以存）。
 *
 * * 空串 **合法**：它表示「恢复默认」，不是「清空成一个坏地址」；
 * * 必须 `https://`：这条管线会把本机审计密文与 Bearer token 发出去，
 *   明文 http 等于把 token 与密文送给链路上的每个人（密文虽不可读，但 token 可被盗用）。
 * * `https://` 之后还得有主机名 —— `https://` 本身能过前缀检查，却发不出去。
 */
export function baseUrlProblem(raw: string): string | null {
  const v = raw.trim();
  if (v === "") return null; // 空串 = 恢复默认
  if (!/^https:\/\//i.test(v)) {
    return "必须以 https:// 开头（明文 http 会把上传 token 暴露给链路上的任何人）";
  }
  const rest = v.slice("https://".length);
  if (rest === "" || rest.startsWith("/") || /\s/.test(rest)) {
    return "https:// 后面要有主机名（例如 https://xraytun.top）";
  }
  return null;
}

/** 配置是否齐备。**只有 `ready` 才允许说「已配置完成」**。 */
export type AuditSyncConfigState = "off" | "incomplete" | "ready";

export interface AuditSyncConfig {
  state: AuditSyncConfigState;
  /**
   * 离「真的能上传」还差什么（一句一条，人话）。
   * `ready` 时为空；`off` 时只说明「开关是关的」（关着时列密钥/token 是噪音）。
   */
  gaps: string[];
}

/**
 * 「这台机器现在算不算配好了」。
 *
 * 为什么需要它：开关是**用户的意图**，不等于**这台机器真的能传**。token 没配时
 * 服务器会直接 401；密钥没生成时密文根本做不出来。旧式做法（只把开关点亮、
 * 别的不提）会让用户以为「开了就自动传了」—— 那正是这一节最该防的错误信念。
 */
export function auditSyncConfig(s: AuditSyncStatus): AuditSyncConfig {
  if (!s.enabled) {
    return { state: "off", gaps: ["自动同步开关是关的（关闭时不会有任何上传请求）"] };
  }
  const gaps: string[] = [];
  if (!s.token_present) {
    gaps.push("上传 token 还没配置：服务器会拒绝上传（401），同步不会成功");
  }
  if (!s.key_present) {
    gaps.push("加密密钥还没生成：没有它做不出密文，已上传的密文也解不开");
  }
  const bad = baseUrlProblem(s.base_url);
  if (bad) gaps.push(`上传地址不可用：${bad}`);
  return { state: gaps.length === 0 ? "ready" : "incomplete", gaps };
}

/**
 * 意图页那一行的**唯一判据**（`docs/design/AUDIT-SYNC.md` §8）。
 *
 * 四种状态各自一句话，且**读不到 ≠ 关闭**：`null` 只说读不到（附原因），
 * 不许退化成「关闭」或「从未成功」。
 */
export function auditSyncStatusLine(s: AuditSyncStatus | null, unknown: string): string {
  if (s === null) return `审计同步：读不到状态（${unknown}）—— 空不等于没在传`;
  if (!s.enabled) return "审计同步：关闭（关闭时不会有任何上传请求）";
  const pending = s.pending_days.length;
  const tail = pending > 0 ? `，待传 ${pending} 天` : "，当前没有待传的天";
  if (s.last_ok_unix === null) return `审计同步：从未成功上传过${tail}`;
  const day = s.last_uploaded_day ?? "（未知日期）";
  return `审计同步：已上传到 ${day}${tail}`;
}

/** Unix 秒 → 本机时区的 `YYYY-MM-DD HH:MM`；`null` 返回 `null`（调用方自己说人话）。 */
export function fmtStamp(unix: number | null): string | null {
  if (unix === null || !Number.isFinite(unix)) return null;
  const d = new Date(unix * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return (
    `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ` +
    `${p(d.getHours())}:${p(d.getMinutes())}`
  );
}

/**
 * 「立即同步一次」的结果文案。
 *
 * 成功与失败**必须能分开读**：即使已经传上去几天，只要这一轮最后失败了，
 * 也要把失败原因说出来（不许因为 `uploaded.length > 0` 就渲染成成功）。
 */
export function auditSyncNowText(run: AuditSyncRun): string {
  const days = run.uploaded.join("、");
  if (run.error) {
    return run.uploaded.length > 0
      ? `已上传 ${run.uploaded.length} 天（${days}），但随后失败：${run.error}`
      : `同步失败，一天都没传上去：${run.error}`;
  }
  if (run.uploaded.length === 0) {
    // 只有「这次一天都没传」这一个事实，**不猜**为什么（退避？没有待传？后端没给原因）。
    return "这次没有上传任何一天。";
  }
  return `已上传 ${run.uploaded.length} 天：${days}`;
}

/**
 * 「撤回全部已上传」的结果文案。
 *
 * `error` 非空时**只**说失败：`deleted` 那一刻没有服务端确认，说成「已删除」
 * 就是把一次失败讲成一次成功。
 */
export function auditSyncRevokeText(r: AuditSyncRevoke): string {
  if (r.error) return `撤回失败：${r.error}（服务端没有确认删除任何对象）`;
  if (r.deleted <= 0) return "服务端确认：你没有已上传的对象（删除 0 个）";
  return `服务端已确认删除 ${r.deleted} 个对象`;
}

export interface AuditSyncStatusSource {
  /** 后端给的状态；`null` = 还没读回来**或**读失败（看 `statusError`）。 */
  status: AuditSyncStatus | null;
  /** 读失败的人话原因；`null` = 没失败。 */
  statusError: string | null;
  /** 第一次读取还没回来（`status === null && statusError === null`）。 */
  loading: boolean;
  /** 重新读一次（动作之后调用；动作本身返回的新状态也可以直接 `adopt`）。 */
  refresh: () => Promise<void>;
  /** 直接用动作返回的状态（省一次往返，且与动作结果同源）。 */
  adopt: (status: AuditSyncStatus) => void;
}

/**
 * 审计同步状态的**唯一请求入口**。
 *
 * `enabled: false` 时**不发请求**：设置页只在切到「审计同步」那一节时才需要它，
 * 别的分类不该为一个看不见的卡片付一次 IPC。
 *
 * `pollMs` > 0 时按间隔重读（意图页要保持那一行新鲜；设置页不需要）。
 */
export function useAuditSyncStatus(
  opts: { enabled?: boolean; pollMs?: number } = {},
): AuditSyncStatusSource {
  const { enabled = true, pollMs } = opts;
  const [status, setStatus] = useState<AuditSyncStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const next = await api.auditSyncStatus();
      setStatus(next);
      setStatusError(null);
    } catch (e) {
      // 读失败 ⇒ 状态作废（不许留上一轮的值冒充当前值）。
      setStatus(null);
      setStatusError(errorText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!enabled) return;
    void refresh();
    if (!pollMs || pollMs <= 0) return;
    const t = window.setInterval(() => void refresh(), pollMs);
    return () => window.clearInterval(t);
  }, [enabled, refresh, pollMs]);

  const adopt = useCallback((next: AuditSyncStatus) => {
    setStatus(next);
    setStatusError(null);
    setLoading(false);
  }, []);

  return { status, statusError, loading, refresh, adopt };
}
