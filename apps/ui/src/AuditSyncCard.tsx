/**
 * 「审计同步」卡片（设置页 · 系统与助手）。
 *
 * 需求与文案规矩见 `docs/design/AUDIT-SYNC.md` §5–§8。这一张卡要回答三件事：
 * 开关（默认关，关闭时后端零请求）、**将要上传什么**（本机明文预览，不联网）、
 * **传到哪一步了**（设备 id / 最后成功 / 待传 / 最后错误 / 退避）。
 *
 * # 这一卡最容易犯的错（都被测试钉着）
 *
 * * 把「开关亮着」说成「配置完成」—— token 没配时服务器会 401，同步根本不会成功；
 * * `last_ok_unix === null` 时说「正常」—— 我们**没有任何成功证据**；
 * * 撤回失败（`error` 非空）时说「已删除」—— `deleted` 那一刻没有服务端确认。
 *
 * 这里的每个动作都用**本组件自己的 `busy`**（不借 `run()`）：`run()` 在别处忙时
 * 会直接丢弃点击，而「立即同步」被静默吞掉，用户会以为传过了。
 */
import { useState } from "react";

import {
  DEFAULT_AUDIT_BASE_URL,
  auditSyncConfig,
  auditSyncNowText,
  auditSyncRevokeText,
  baseUrlProblem,
  fmtStamp,
  useAuditSyncStatus,
} from "./auditSync";
import { InlineConfirm } from "./InlineConfirm";
import { CopyButton } from "./IncidentReport";
import { api, errorText } from "./ipc";
import type { AuditSyncPreview, AuditSyncRevoke, AuditSyncRun } from "./types";

/** 待传/跳过的天列表最多直接列几个，其余用「…等 N 天」带过。 */
function dayList(days: string[], max = 8): string {
  if (days.length <= max) return days.join("、");
  return `${days.slice(0, max).join("、")}…等 ${days.length} 天`;
}

export function AuditSyncCard() {
  const { status, statusError, refresh, adopt } = useAuditSyncStatus();

  /** 上传地址编辑框；`null` = 跟随后端值（没在编辑）。 */
  const [baseDraft, setBaseDraft] = useState<string | null>(null);
  /** token 输入框（`type=password`，永不回显后端已存的值）。 */
  const [tokenDraft, setTokenDraft] = useState("");
  /** 本卡自己的进行中动作（`null` = 空闲）。 */
  const [busy, setBusy] = useState<
    null | "toggle" | "base" | "token" | "clear-token" | "now" | "preview" | "revoke"
  >(null);
  /** 动作级失败原因（IPC reject 那一类；后端给出的失败原因走各自的返回值）。 */
  const [actionError, setActionError] = useState<string | null>(null);
  const [nowRun, setNowRun] = useState<AuditSyncRun | null>(null);
  const [preview, setPreview] = useState<AuditSyncPreview | null>(null);
  const [revoke, setRevoke] = useState<AuditSyncRevoke | null>(null);

  const runAction = async (
    kind: "toggle" | "base" | "token" | "clear-token" | "now" | "preview" | "revoke",
    action: () => Promise<void>,
  ) => {
    setBusy(kind);
    setActionError(null);
    try {
      await action();
    } catch (e) {
      setActionError(errorText(e));
    } finally {
      setBusy(null);
    }
  };

  // ---- 读不到状态：只说读不到，不编「关闭/从未成功」----
  if (status === null) {
    return (
      <>
        <h2 className="card__title">审计同步</h2>
        {statusError ? (
          <div className="banner banner--error">
            读不到审计同步状态：{statusError}
            <div style={{ marginTop: 8 }}>
              <button className="btn" onClick={() => void refresh()}>
                重试
              </button>
            </div>
          </div>
        ) : (
          <p className="empty">正在读取审计同步状态…</p>
        )}
      </>
    );
  }

  const cfg = auditSyncConfig(status);
  const base = baseDraft ?? status.base_url;
  const baseProblem = baseUrlProblem(base);
  const baseChanged = base.trim() !== status.base_url.trim();
  const pending = status.pending_days ?? [];
  const skipped = status.skipped_days ?? [];
  const lastOk = fmtStamp(status.last_ok_unix);
  const lastAttempt = fmtStamp(status.last_attempt_unix);
  const nextRetry = fmtStamp(status.next_retry_unix);

  return (
    <>
      <h2 className="card__title">审计同步</h2>
      <p className="card__desc">
        把「意图判定审计」按天打包，在<strong>本机</strong>用设备密钥加密后上传到我们自己的服务器。
        上传的是<strong>密文</strong>，服务器看不到域名。默认<strong>关闭</strong>。
      </p>

      {/* ---- 开关 ---- */}
      <label className="row" style={{ fontSize: 12 }}>
        <input
          type="checkbox"
          checked={status.enabled}
          disabled={busy !== null}
          onChange={(e) =>
            void runAction("toggle", async () => {
              adopt(await api.auditSyncSetEnabled(e.target.checked));
            })
          }
        />
        自动同步（每台设备每天一次）
      </label>

      {status.enabled ? (
        <div className="field__hint" style={{ marginBottom: 10 }}>
          现在<strong>开着</strong>。代价写在这里：服务端仍能看到{" "}
          <strong>设备随机 id、日期、行数、密文长度、上传时刻</strong>
          （这些足以做流量模式分析，所以<strong>不是</strong>「完全匿名」）。
          加密密钥只在本机 Keychain：<strong>密钥丢失 ⇒ 已上传的密文永久读不出</strong>
          （只能撤回删除）；本机的 <span className="mono">intent-audit.jsonl</span>{" "}
          始终是第一副本，历史不会因此丢。
        </div>
      ) : (
        <div className="field__hint" style={{ marginBottom: 10 }}>
          现在<strong>关着</strong>：开启后每台设备每天自动上传一次，内容是
          <strong>端到端加密的密文</strong>，服务器看不到域名。关闭时不会有任何上传请求。
        </div>
      )}

      {/* 配置不完整：说清差什么，而不是让用户以为「开关亮了就在传」。 */}
      {cfg.state === "incomplete" && (
        <div className="banner banner--warn">
          <div>
            开关开着，但这台机器<strong>还没配好</strong>，同步不会成功：
            <ul className="list">
              {cfg.gaps.map((g) => (
                <li key={g}>{g}</li>
              ))}
            </ul>
          </div>
        </div>
      )}

      {/* ---- 上传地址 ---- */}
      <div className="field">
        <label htmlFor="audit-base-url">上传地址（必须 https://；留空 = 恢复默认）</label>
        <div className="row">
          <input
            id="audit-base-url"
            className="input mono"
            style={{ flex: 1, minWidth: 0 }}
            value={base}
            placeholder={DEFAULT_AUDIT_BASE_URL}
            onChange={(e) => setBaseDraft(e.target.value)}
          />
          <button
            className="btn"
            // 非法地址**不能存**：明文 http 会把 token 送给链路上的每个人。
            disabled={busy !== null || baseProblem !== null || !baseChanged}
            onClick={() =>
              void runAction("base", async () => {
                adopt(await api.auditSyncSetBaseUrl(base.trim()));
                setBaseDraft(null);
              })
            }
          >
            {base.trim() === "" ? "恢复默认" : "保存地址"}
          </button>
        </div>
        {baseProblem !== null && (
          <div className="field__hint" style={{ color: "var(--danger, #d9534f)" }}>
            不能保存：{baseProblem}
          </div>
        )}
        <div className="field__hint">
          当前生效：<span className="mono">{status.base_url || DEFAULT_AUDIT_BASE_URL}</span>
        </div>
      </div>

      {/* ---- 上传 token ---- */}
      <div className="field">
        <label htmlFor="audit-token">上传 token</label>
        {status.token_present ? (
          <div className="row row--wrap">
            <strong>已配置（不回显）</strong>
            <button
              className="btn"
              disabled={busy !== null}
              onClick={() =>
                void runAction("clear-token", async () => {
                  adopt(await api.auditSyncSetToken(""));
                })
              }
            >
              清除
            </button>
          </div>
        ) : (
          <div className="row">
            <input
              id="audit-token"
              type="password"
              className="input mono"
              style={{ flex: 1, minWidth: 0 }}
              value={tokenDraft}
              placeholder="粘贴 Worker 侧设置的 AUDIT_TOKEN"
              onChange={(e) => setTokenDraft(e.target.value)}
            />
            <button
              className="btn"
              disabled={busy !== null || tokenDraft.trim() === ""}
              onClick={() =>
                void runAction("token", async () => {
                  adopt(await api.auditSyncSetToken(tokenDraft.trim()));
                  setTokenDraft("");
                })
              }
            >
              保存 token
            </button>
          </div>
        )}
        <div className="field__hint">
          token 存在本机 Keychain，界面不会回显。没有它，服务器会拒绝上传（401）。
        </div>
      </div>

      {/* ---- 状态区 ---- */}
      <h3>状态</h3>
      <div className="kv">
        <span>设备 id</span>
        <strong className="mono">
          {status.device || "还没初始化（首次开启同步时生成，不是硬件指纹）"}
        </strong>
      </div>
      {status.device !== "" && (
        <div className="row" style={{ marginBottom: 8 }}>
          <CopyButton label="复制设备 id" text={status.device} className="btn btn--ghost" />
        </div>
      )}
      <div className="kv">
        <span>加密密钥</span>
        <strong>{status.key_present ? "已在本机 Keychain 生成" : "还没生成"}</strong>
      </div>
      <div className="kv">
        <span>最后成功上传</span>
        {/* 没有成功证据就只能说「从未成功上传过」—— 不许说「正常」。 */}
        <strong>
          {lastOk === null
            ? "从未成功上传过"
            : `${lastOk}${status.last_uploaded_day ? `（传到 ${status.last_uploaded_day}）` : ""}`}
        </strong>
      </div>
      <div className="kv">
        <span>最后一次尝试</span>
        <strong>{lastAttempt ?? "还没尝试过"}</strong>
      </div>
      <div className="kv">
        <span>待传天数</span>
        <strong>{pending.length === 0 ? "没有待传的天" : `${pending.length} 天`}</strong>
      </div>
      {pending.length > 0 && <p className="field__hint">待传：{dayList(pending)}</p>}
      {skipped.length > 0 && (
        <p className="field__hint">
          因为太老被跳过、不再重试（{skipped.length} 天）：{dayList(skipped)}
        </p>
      )}
      {status.last_error !== null && (
        <div className="banner banner--error">最后一次失败的原因：{status.last_error}</div>
      )}
      {nextRetry !== null && (
        <p className="field__hint">
          退避中：下一次重试大约在 {nextRetry}（失败越多次等得越久，上限 6 小时）。
        </p>
      )}

      {/* ---- 动作 ---- */}
      <div className="row row--wrap" style={{ marginTop: 10 }}>
        <button
          className="btn btn--primary"
          disabled={busy !== null}
          onClick={() =>
            void runAction("now", async () => {
              setRevoke(null);
              const run = await api.auditSyncNow();
              setNowRun(run);
              adopt(run.status);
            })
          }
        >
          {busy === "now" ? "正在同步…" : "立即同步一次"}
        </button>
        <button
          className="btn"
          disabled={busy !== null}
          onClick={() =>
            void runAction("preview", async () => {
              setPreview(await api.auditSyncPreview(null));
            })
          }
        >
          {busy === "preview" ? "正在生成预览…" : "查看将要上传的内容"}
        </button>
        <InlineConfirm
          label="撤回全部已上传"
          className="btn btn--danger"
          disabled={busy !== null}
          question="确认撤回？服务端会删除这台设备已上传的全部审计密文，删掉后无法恢复（本机的 intent-audit.jsonl 不受影响）。"
          confirmLabel="确认撤回"
          onConfirm={() =>
            void runAction("revoke", async () => {
              const r = await api.auditSyncRevoke();
              setRevoke(r);
              // 只有服务端确认删了才重读状态（失败时状态没变，也免得看起来像刷新成功）。
              if (!r.error) await refresh();
            })
          }
        />
      </div>

      {actionError !== null && (
        <div className="banner banner--error" style={{ marginTop: 10 }}>
          {actionError}
        </div>
      )}

      {nowRun !== null && (
        <div
          className={`banner ${nowRun.error ? "banner--error" : "banner--info"}`}
          role="status"
          style={{ marginTop: 10 }}
        >
          {auditSyncNowText(nowRun)}
        </div>
      )}

      {revoke !== null && (
        <div
          className={`banner ${revoke.error ? "banner--error" : "banner--info"}`}
          role="status"
          style={{ marginTop: 10 }}
        >
          {auditSyncRevokeText(revoke)}
        </div>
      )}

      {/* ---- 预览（本机数据，不联网）---- */}
      {preview !== null && (
        <div className="conn-detail" style={{ marginTop: 10 }}>
          <h3>将要上传的内容</h3>
          <p className="field__hint">
            <strong>这是本机数据，预览不联网</strong> —— 生成预览不会发出任何上传请求。
          </p>
          {preview.day === null || preview.plaintext === null ? (
            <p className="empty">{preview.note ?? "现在没有可预览的天。"}</p>
          ) : (
            <>
              <div className="kv">
                <span>天</span>
                <strong className="mono">{preview.day}</strong>
              </div>
              <div className="kv">
                <span>行数 / 字节</span>
                <strong>
                  {preview.rows} 行 · {preview.bytes} 字节
                </strong>
              </div>
              {preview.note !== null && <p className="field__hint">{preview.note}</p>}
              <details className="page__details">
                <summary>明文 bundle JSON（本机数据，预览不联网）</summary>
                <pre className="mono" style={{ maxHeight: 320, overflow: "auto" }}>
                  {preview.plaintext}
                </pre>
              </details>
            </>
          )}
        </div>
      )}
    </>
  );
}
