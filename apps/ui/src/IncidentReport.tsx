/**
 * 「报告问题」：看清单 → 确认上传 → 显示/复制编号（task-131）。
 *
 * # 放哪儿、为什么放这儿
 *
 * 卡里给了两个候选（「设置 → 系统与助手」或「日志」页），我选了**日志页**，
 * 在「诊断报告」那块**下面**挂一个独立分节：
 *
 * * 用户要报的问题，证据就是**这一页的日志**（还有诊断报告）。把他从证据旁边挪到
 *   设置页深处去点上传，等于让他在两个页面之间来回确认「包里到底有什么」；
 * * 「报告问题」的第一步（`incident_preview()`）产出的就是「日志 + 状态」的快照，
 *   与这一页的职责是同一条链；
 * * 设置页的「系统与助手」已经很长（helper 状态/版本对照/安装卸载），再塞一条
 *   **会联网上传**的流程，容易和「修复网络」「卸载 helper」这类高危动作混在一起。
 *
 * 挂载点刻意**避开** `Logs.tsx` 里「脱敏说明」那两处（约 185-197，归 task-124）。
 *
 * # 隐私红线（不许静默上传）
 *
 * `incident_preview()` 只**在本地**打包（不上传），结果必须**先**渲染出来：
 * 包内清单 + 脱敏说明（`readme`）+ 被截断的文件。`确认上传` 只在拿到预览之后才存在，
 * 所以「先看再传」不是靠文案约束，而是**结构上做不到**跳过。
 *
 * # 组件是纯的
 *
 * 不碰 `useStore`：它只经 `ipc.ts` 调命令，所以测试可以直接 `render(<IncidentReport />)`
 * 注入替身，覆盖 201 / 422 / 网络失败 / 5xx 四条路径。
 */
import { useCallback, useEffect, useState } from "react";

import { api, errorText } from "./ipc";
import {
  formatSecretHit,
  parseIncidentFailure,
  type IncidentFailure,
  type IncidentPreview,
  type IncidentUpload,
} from "./incident";
import { formatServerTime } from "./incident";
import { formatBytes } from "./types";

/**
 * 复制按钮：**失败必须可见**，并给一个可手动选中的文本区。
 *
 * 起因是 task-128 的诚实清单里点名的那条行为级缺陷：设置页的
 * `navigator.clipboard.writeText(...).catch(() => console.log(text))` —— 剪贴板被拒时
 * 界面**毫无反应**，用户以为复制成功，贴出去是空的（或什么都没有）。
 *
 * `load` 是可选的异步取文本（设置页的诊断报告要点一下才生成）；取文本失败与
 * 写剪贴板失败**都会**显示出来 —— 两者都是「你以为成功了，其实没有」。
 */
export function CopyButton({
  label,
  text,
  load,
  disabled,
  className = "btn",
}: {
  label: string;
  /** 已经拿到的文本。 */
  text?: string;
  /** 或者：点的时候再去取（取失败也会显示出来）。 */
  load?: () => Promise<string>;
  disabled?: boolean;
  className?: string;
}) {
  const [state, setState] = useState<
    { phase: "idle" } | { phase: "busy" } | { phase: "ok" } | { phase: "failed"; why: string; fallback: string }
  >({ phase: "idle" });

  const run = async () => {
    setState({ phase: "busy" });
    let value = text ?? "";
    if (load) {
      try {
        value = await load();
      } catch (e) {
        setState({ phase: "failed", why: `生成要复制的文本失败：${errorText(e)}`, fallback: "" });
        return;
      }
    }
    try {
      await navigator.clipboard.writeText(value);
      setState({ phase: "ok" });
    } catch {
      // 剪贴板可能被拒（权限/非聚焦窗口）。**不许静默**：说清失败，并把文本
      // 放进一个可手动选中的 textarea —— 复制不了至少还能自己选中拷贝。
      setState({
        phase: "failed",
        why: "剪贴板不可用（权限被拒或窗口未聚焦），没有复制成功。",
        fallback: value,
      });
    }
  };

  return (
    <>
      <button
        type="button"
        className={className}
        disabled={disabled || state.phase === "busy"}
        title="复制不了时会给出一个可手动选中的文本区"
        onClick={() => void run()}
      >
        {state.phase === "busy" ? "正在复制…" : label}
      </button>
      {state.phase === "ok" && (
        <span className="field__hint" role="status">
          已复制到剪贴板。
        </span>
      )}
      {state.phase === "failed" && (
        <div className="banner banner--warn" role="alert" style={{ marginTop: 8, flexBasis: "100%" }}>
          <span>⚠︎</span>
          <div>
            <strong>复制失败：</strong>
            {state.why}
            {state.fallback ? (
              <>
                <div style={{ marginTop: 6 }}>下面这段可以直接手动选中复制：</div>
                {/* `readOnly` + 自动全选：用户点一下就能 Ctrl/Cmd+C */}
                <textarea
                  className="mono"
                  readOnly
                  aria-label="手动复制内容"
                  value={state.fallback}
                  rows={4}
                  style={{ width: "100%", marginTop: 6, resize: "vertical" }}
                  onFocus={(e) => e.currentTarget.select()}
                />
              </>
            ) : null}
          </div>
        </div>
      )}
    </>
  );
}

type Phase = "idle" | "loading" | "preview" | "uploading" | "done";

/**
 * 哨兵角标的刷新间隔（task-149）。
 *
 * # 为什么是 30 秒、为什么不挂在别处
 *
 * `incident_anomaly_count()` **只是在本地读一个文件**（不走网络、不进核心），
 * 所以 30 秒一次的成本可以忽略；但它反映的是「现在有多少条待上报」——
 * 只在挂载时对一次的话，用户开着这一页会看到「有 0 条」而以为没出事，
 * 这正是本项目最忌的「你以为没事」。
 *
 * **刻意不绑到 2 秒级的快照轮询上**：那是另一个 cadence（拓扑/状态刷新），
 * 为了一个角标把它抬到 2 秒既没必要，也会让「刷新频率」这件事在代码里变得含糊。
 *
 * 刷新时机（四条，全部在下面的 effect 里）：
 * 1. 挂载；
 * 2. 页面 `visibilitychange` 变可见 / 窗口重新获得焦点；
 * 3. 上传成功后（`confirmUpload` 里直接调一次）；
 * 4. 页面可见时每 30 秒一次 —— **不可见就停掉**（不留后台 timer）。
 */
export const ANOMALY_REFRESH_MS = 30_000;

export default function IncidentReport() {
  const [phase, setPhase] = useState<Phase>("idle");
  const [preview, setPreview] = useState<IncidentPreview | null>(null);
  const [upload, setUpload] = useState<IncidentUpload | null>(null);
  const [failure, setFailure] = useState<IncidentFailure | null>(null);
  const [anomalies, setAnomalies] = useState<number | null>(null);

  /**
   * 被动哨兵计数：**只在本地**读，不触发采集、更不触发上传。
   * 调用点全部包在 try/catch 里 —— 测试替身或旧后端没有这个命令时，
   * 角标**不显示**（读不到就不显示），而不是让整页崩掉。
   */
  const loadCount = useCallback(async () => {
    try {
      const n = await api.incidentAnomalyCount();
      setAnomalies(typeof n === "number" && Number.isFinite(n) ? n : null);
    } catch {
      setAnomalies(null);
    }
  }, []);

  /**
   * 角标的刷新调度（task-149）：挂载 / 变可见 / 获得焦点 ⇒ 立刻读一次；
   * **只在页面可见时**起 30 秒的定时器，不可见或卸载都清掉（不留后台 timer）。
   *
   * `visibilityState` 与 `focus` 在真机 WKWebView 里的行为未验证（见诚实清单），
   * 这里用的是标准 DOM API，jsdom 下可注入/可测。
   */
  useEffect(() => {
    let timer: number | null = null;
    const stopTimer = () => {
      if (timer !== null) {
        window.clearInterval(timer);
        timer = null;
      }
    };
    const startTimer = () => {
      stopTimer();
      timer = window.setInterval(() => void loadCount(), ANOMALY_REFRESH_MS);
    };
    const onVisibleOrFocused = () => {
      if (document.visibilityState === "hidden") {
        // 不可见 ⇒ 不读、也不留定时器（后台标签页不该有 timer 在跑）
        stopTimer();
        return;
      }
      void loadCount();
      startTimer();
    };

    void loadCount(); // ① 挂载先读一次
    if (document.visibilityState !== "hidden") startTimer(); // ④ 可见才起表
    document.addEventListener("visibilitychange", onVisibleOrFocused);
    window.addEventListener("focus", onVisibleOrFocused);
    return () => {
      stopTimer();
      document.removeEventListener("visibilitychange", onVisibleOrFocused);
      window.removeEventListener("focus", onVisibleOrFocused);
    };
  }, [loadCount]);

  const collect = async () => {
    setPhase("loading");
    setFailure(null);
    setUpload(null);
    try {
      setPreview(await api.incidentPreview());
      setPhase("preview");
    } catch (e) {
      setPreview(null);
      setPhase("idle");
      setFailure({
        kind: "unknown",
        message: "本地打包失败，没有生成清单，也就没有上传任何东西。",
        next: "稍后重试；持续失败请把下面这段原文反馈给开发者。",
        hits: [],
        raw: errorText(e),
      });
    }
  };

  const confirmUpload = async () => {
    if (!preview) return;
    setPhase("uploading");
    setFailure(null);
    try {
      const r = await api.incidentUpload(preview.bundle_path);
      setUpload(r);
      setPhase("done");
      void loadCount();
    } catch (e) {
      setFailure(parseIncidentFailure(e));
      // 回到预览态：包还在本地，用户能看清清单后再决定重试或放弃。
      setPhase("preview");
    }
  };

  const reset = () => {
    setPhase("idle");
    setPreview(null);
    setUpload(null);
    setFailure(null);
  };

  const badge =
    anomalies !== null && anomalies > 0 ? `有 ${anomalies} 条待上报` : null;
  /** 服务器时间（ISO 串 ⇒ 本地化显示）；解析不出来就是 `null`（不渲染）。 */
  const serverTime = upload ? formatServerTime(upload.received_at) : null;

  return (
    <section className="page__sec">
      <h2 className="page__title">报告问题</h2>
      <p className="page__desc">
        点「报告问题」只会在<strong>本地</strong>打一个包（日志 + 运行状态），
        <strong>不会自动上传</strong>。打完会把「包里有哪些文件、脱敏做了什么、
        哪些内容被截断」先给你看一遍；你点「确认上传」之后才会发出去。
      </p>

      {/* 角标在任何阶段都显示（它说的是**状态**，不是某个阶段的信息）：
          「有 N 条待上报」= 本地哨兵已经记下的异常条数；刷新时机见上面 `ANOMALY_REFRESH_MS` 的注释。 */}
      {badge && (
        <div className="row row--wrap" style={{ marginTop: 8 }}>
          <span className="badge badge--unknown" title="只统计本地记录，上传仍然只由你点击触发">
            {badge}
          </span>
        </div>
      )}

      {phase === "idle" && (
        <div className="row row--wrap">
          <button className="btn btn--primary" onClick={() => void collect()}>
            报告问题
          </button>
        </div>
      )}

      {phase === "loading" && <div className="empty">正在本地收集信息并生成清单…</div>}

      {preview && (phase === "preview" || phase === "uploading") && (
        <>
          <div className="kv" style={{ marginTop: 10 }}>
            <div>
              <div className="kv__k">包大小</div>
              <div className="kv__v mono">{formatBytes(preview.size_bytes)}</div>
            </div>
            <div>
              <div className="kv__k">文件数</div>
              <div className="kv__v mono">{preview.files.length}</div>
            </div>
          </div>

          <div style={{ marginTop: 12 }}>
            <strong>包内清单</strong>
            <div className="list">
              {preview.files.map((f) => (
                <div className="list__row" key={f.name} style={{ cursor: "default" }}>
                  <div className="list__main">
                    <div className="list__name mono">{f.name}</div>
                    <div className="list__meta mono">
                      {formatBytes(f.bytes)} · sha256 {f.sha256.slice(0, 16)}…
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </div>

          {preview.readme && (
            <div style={{ marginTop: 12 }}>
              <strong>脱敏说明</strong>
              <pre className="code-block" style={{ whiteSpace: "pre-wrap", marginTop: 6 }}>
                {preview.readme}
              </pre>
            </div>
          )}

          {/* **被截断的文件必须说出来**：悄悄少给内容，等于让用户以为「包里就是全部」 */}
          {preview.truncated.length > 0 && (
            <div className="note" style={{ marginTop: 10 }}>
              有 {preview.truncated.length} 个文件<strong>因为太大被截断</strong>（只带了开头部分）：
              <span className="mono"> {preview.truncated.join("、")}</span>
            </div>
          )}

          <div className="row row--wrap" style={{ marginTop: 12 }}>
            <button
              className="btn btn--primary"
              disabled={phase === "uploading"}
              onClick={() => void confirmUpload()}
            >
              {phase === "uploading" ? "正在上传…" : "确认上传"}
            </button>
            <button className="btn btn--ghost" disabled={phase === "uploading"} onClick={reset}>
              先不上传
            </button>
          </div>
        </>
      )}

      {/* `received_at` 是 **ISO 8601 串**（Lead 裁决，依据 `src/worker.mjs:263` 的
          `new Date(nowMs).toISOString()`）。原来这里是「只有 number 才渲染 ⇒ 永远不显示」；
          现在交给 `formatServerTime`：解析不出有限值就整段不渲染（**一个时间都不编**）。 */}
      {phase === "done" && upload && (
        <div className="note" style={{ marginTop: 10 }}>
          <strong>已上传。</strong>编号：
          <span className="mono"> {upload.id}</span>
          <div className="field__hint" style={{ marginTop: 6 }}>
            {formatBytes(upload.bytes)} · sha256 {upload.sha256.slice(0, 16)}…
            {serverTime !== null ? ` · 服务器时间 ${serverTime}` : ""}
          </div>
          <div className="row row--wrap" style={{ marginTop: 8 }}>
            <CopyButton label="复制编号" text={upload.id} />
            <button className="btn btn--ghost" onClick={reset}>
              完成
            </button>
          </div>
        </div>
      )}

      {failure && (
        <div className="banner banner--warn" role="alert" style={{ marginTop: 10 }}>
          <span>⚠︎</span>
          <div>
            <strong>{failure.message}</strong>
            <div style={{ marginTop: 4 }}>{failure.next}</div>
            {failure.kind === "secret" && failure.hits.length > 0 && (
              <>
                <div style={{ marginTop: 6 }}>
                  命中位置（<strong>只给位置与类型，不显示密钥内容</strong>）：
                </div>
                <ul className="mono" style={{ margin: "4px 0 0 18px" }}>
                  {failure.hits.map((h, i) => (
                    <li key={`${formatSecretHit(h)}-${i}`}>{formatSecretHit(h)}</li>
                  ))}
                </ul>
              </>
            )}
            {failure.raw && (
              <pre className="code-block" style={{ whiteSpace: "pre-wrap", marginTop: 6 }}>
                {failure.raw}
              </pre>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
