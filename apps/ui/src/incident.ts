/**
 * 「报告问题」链路的**前端契约**（task-131）。
 *
 * # 为什么类型定义在这里，而不是 `types.ts`
 *
 * 后端契约由 Lead 在 task-131 里**冻结**（命令名与返回结构），实现方是 task-130。
 * 而 `apps/ui/src/types.ts` 此刻有另一张卡（task-127）的注释落点，且 `types.ts`
 * 的既有接口被 `previewFidelity.test.ts` / `type_contract.rs` 当成 Rust↔TS 的
 * 对照表 —— 往里塞新结构会牵动那两处。所以这一组**新**的线上类型单独放这里，
 * 等 task-130 落地后由 Lead 决定是否并回 `types.ts`。
 *
 * # 冻结契约（照抄 task-131 卡文）
 *
 * ```text
 * incident_preview()           -> IncidentPreview
 * incident_upload(bundle_path) -> IncidentUpload
 *                                错误：SecretDetected{message,hits[]} | RateLimited | Network | Server{code,message}
 * incident_anomaly_count()     -> number
 * incident_anomalies(limit)    -> Anomaly[]
 * ```
 *
 * ⚠️ **`Anomaly` 的字段没有给**，所以这一版**不调用** `incident_anomalies()` ——
 * 猜一个形状就是「编接口」（已经报给 Lead）。角标只用**字段齐备**的
 * `incident_anomaly_count()`。
 */

/** 包内一个文件。`sha256` 由后端算好，用来让用户核对内容。 */
export interface IncidentFile {
  name: string;
  bytes: number;
  sha256: string;
}

/** `incident_preview()` 的结果：**上传之前**要给用户看的东西在这里。 */
export interface IncidentPreview {
  /** 本地包的绝对路径；`incident_upload` 要的就是它。 */
  bundle_path: string;
  size_bytes: number;
  files: IncidentFile[];
  /** 脱敏说明（纯文本，界面用 `white-space: pre-wrap` 原样显示）。 */
  readme: string;
  /** 包内清单的原始文本（与 `files` 同源，供人眼核对）。 */
  manifest: string;
  /** 因为太大/太多而**被截断**的文件名 —— 必须说出来，不能悄悄少给。 */
  truncated: string[];
}

/** `incident_upload()` 成功的结果。 */
export interface IncidentUpload {
  id: string;
  sha256: string;
  bytes: number;
  received_at: number;
}

/** `SecretDetected` 里的一条命中：**只有位置与类型，没有密钥原文**。 */
export interface IncidentSecretHit {
  file: string;
  line: number;
  kind: string;
}

export type IncidentFailureKind = "secret" | "rate_limited" | "network" | "server" | "unknown";

/** 上传失败的界面口径（纯数据，渲染在组件里）。 */
export interface IncidentFailure {
  kind: IncidentFailureKind;
  /** 一句能看懂的话（**不是**后端原文照抄）。 */
  message: string;
  /** 下一步该做什么 —— 四条路径都必须有。 */
  next: string;
  /** 只有 `secret` 有：`文件:行号:类型`。 */
  hits: IncidentSecretHit[];
  /** 后端原文（`unknown` 时给用户看，便于报障）。 */
  raw: string | null;
}

function asRecord(v: unknown): Record<string, unknown> | null {
  return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : null;
}

function normKind(v: unknown): IncidentFailureKind | null {
  if (typeof v !== "string") return null;
  switch (v.toLowerCase().replace(/[^a-z]/g, "")) {
    case "secretdetected":
    case "secret":
      return "secret";
    case "ratelimited":
    case "rate":
      return "rate_limited";
    case "network":
      return "network";
    case "server":
      return "server";
    default:
      return null;
  }
}

function normHits(v: unknown): IncidentSecretHit[] {
  if (!Array.isArray(v)) return [];
  const out: IncidentSecretHit[] = [];
  for (const item of v) {
    const r = asRecord(item);
    if (!r) continue;
    const file = typeof r.file === "string" ? r.file : null;
    const kind = typeof r.kind === "string" ? r.kind : null;
    if (file === null || kind === null) continue;
    const line = typeof r.line === "number" && Number.isFinite(r.line) ? r.line : 0;
    out.push({ file, line, kind });
  }
  return out;
}

/**
 * 把 `incident_upload()` 的错误翻成界面口径。
 *
 * 后端可能给三种形状，这里**都接受**（并**不修改**契约）：
 * 1. 结构化对象 `{kind:"secret_detected", message, hits}`；
 * 2. JSON 字符串（`invoke` 的 reject 在部分路径上会序列化成字符串）；
 * 3. 别的字符串/异常 —— 退化成 `unknown`，**把原文交出来**让用户能报障。
 *
 * 四条路径**都必须有 `next`**：只说「失败了」而不说下一步，等于把用户留在原地。
 * `SecretDetected` 只渲染 `文件:行号:类型`（`hits` 里本来就没有密钥原文，
 * 这里也不去截取/回显后端 message 里可能夹带的片段 —— message 是**我们自己**写的）。
 */
export function parseIncidentFailure(err: unknown): IncidentFailure {
  let obj: Record<string, unknown> | null = asRecord(err);
  if (obj === null && typeof err === "string") {
    const t = err.trim();
    if (t.startsWith("{")) {
      try {
        obj = asRecord(JSON.parse(t));
      } catch {
        obj = null;
      }
    }
  }
  if (obj === null && err instanceof Error) obj = null;

  const raw =
    typeof err === "string"
      ? err
      : err instanceof Error
        ? err.message
        : obj
          ? JSON.stringify(obj)
          : String(err);

  // 形状 3 的补充：后端也可能只给一个变体名（`"RateLimited"`），照样认。
  const kind = (obj ? normKind(obj.kind) : null) ?? (typeof err === "string" ? normKind(err) : null);

  if (kind === "secret") {
    const hits = obj ? normHits(obj.hits) : [];
    return {
      kind: "secret",
      message: "包内检测到疑似密钥 —— 已阻止上传（没有发出任何数据）。",
      next:
        hits.length > 0
          ? "请按下面的位置自己检查一遍；确认脱敏无误后再点「重新收集」。若你无法定位，先别上传。"
          : "请检查本地日志与配置里的凭据后再试；无法确定就先别上传。",
      hits,
      raw: null,
    };
  }
  if (kind === "rate_limited") {
    return {
      kind: "rate_limited",
      message: "上传请求过频，服务器暂时拒绝了（限流）。",
      next: "等几分钟再点「重新上传」。包还在本地，不会丢。",
      hits: [],
      raw: null,
    };
  }
  if (kind === "network") {
    return {
      kind: "network",
      message: "网络请求没成功（连不上或中途断了）。",
      next: "确认这台机器能上网（必要时先连上代理）再点「重新上传」；本地包还在。",
      hits: [],
      raw: null,
    };
  }
  if (kind === "server") {
    const code = obj && typeof obj.code === "number" ? obj.code : null;
    return {
      kind: "server",
      message: `服务器拒绝了这次上传${code !== null ? `（HTTP ${code}）` : ""}。`,
      next: "这是服务端问题，隔一会儿重试；持续失败请把下面这段原文一起反馈。",
      hits: [],
      raw,
    };
  }
  return {
    kind: "unknown",
    message: "上传失败（没能识别出原因）。",
    next: "可以稍后重试；如果一直失败，请把下面这段原文反馈给开发者。",
    hits: [],
    raw,
  };
}

/** `文件:行号:类型` —— 仅此三项，**不回显密钥原文**。 */
export function formatSecretHit(h: IncidentSecretHit): string {
  return `${h.file}:${h.line}:${h.kind}`;
}
