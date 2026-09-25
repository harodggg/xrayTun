/**
 * 「失败」怎么让用户看懂：**人话原因 + 下一步动作**。
 *
 * # 这一文件防的两类假陈述
 *
 * 1. **把不知道说成知道**：`invoke` reject 的载荷不保证是字符串。
 *    `JSON.stringify({})` 是 `"{}"`、循环引用会退化成 `String(e)` = `"[object Object]"`
 *    —— 用户看到的就是一句既不是原因也不是办法的乱码。更糟的是另一头：
 *    MITM 状态读失败时，旧界面把「读不到」渲染成「代理没在跑 / 证书还没生成过」，
 *    **把一次 IPC 故障说成了两项确定事实**。
 * 2. **只报错不给路**：一个错误码不是「下一步」。用户要的是「换节点 / 重装助手 /
 *    看日志」里至少一条**他马上能做**的动作。
 *
 * # 口径
 *
 * * `humanError()`：**只**从载荷里读出真实文案（`message` / `reason` / `detail` …），
 *   读不出来就**明说读不出来**，绝不拿 `{}` / `[object Object]` / 裸 JSON 冒充原因；
 * * `nextSteps()`：按**后端自己给出的文案**里出现的线索给建议，**不解析事件 notice、
 *   不按时间猜**；线索命中不了就给唯一的万能动作「看日志」——
 *   宁可少说，也不编一个具体原因。
 * * 两个函数都是**纯函数**，单测直接钉（见 `failureHonesty.test.tsx`）。
 */

/** 一次失败的完整说法：原因 + 可执行动作。 */
export interface FailureAdvice {
  /** 给用户看的原因（永远不是 `[object Object]`，也不是裸 JSON）。 */
  text: string;
  /**
   * 下一步可做的事。**可能只有「看日志」这一条**（那就只有这一条）——
   * 线索不足时不许编具体归因。
   */
  steps: string[];
}

const NO_REASON = "命令失败，但后端没有给出原因";

/** 这些键按优先级找「人能读的原因」；后端把它的人话放在哪个键上都兜得住。 */
const REASON_KEYS = ["message", "reason", "detail", "msg", "description", "cause", "error"] as const;

/** 机器字段：只有码、没有说明时，说出来但明说「只有码」。 */
const CODE_KEYS = ["code", "kind", "status", "error_code"] as const;

const isObjectLike = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

/** 超长 JSON 串裁剪：原因是给人看的，不是给人读日志的。 */
function clip(s: string, max = 300): string {
  return s.length > max ? `${s.slice(0, max)}…` : s;
}

/**
 * 从任意载荷里**取出**一条人能读的原因；取不到就是 `null`（**不推测**）。
 */
function extractReason(e: unknown, depth: number): string | null {
  if (typeof e === "string") {
    const t = e.trim();
    return t.length > 0 ? t : null;
  }
  if (e instanceof Error) {
    const t = e.message.trim();
    return t.length > 0 ? t : null;
  }
  if (Array.isArray(e)) {
    const parts = e.map((x) => extractReason(x, depth + 1)).filter((x): x is string => x !== null);
    return parts.length > 0 ? parts.join("；") : null;
  }
  if (isObjectLike(e) && depth < 3) {
    for (const key of REASON_KEYS) {
      const r = extractReason(e[key], depth + 1);
      if (r !== null) return r;
    }
    for (const key of CODE_KEYS) {
      const code = e[key];
      if (typeof code === "string" || typeof code === "number") {
        return `后端只给了错误码 ${String(code)}，没有说明原因`;
      }
    }
  }
  return null;
}

/**
 * 把任意 reject 载荷翻译成**一句人话**。
 *
 * 重要：`null` / `undefined` / `{}` 都是「后端没给原因」的**真实形态**，
 * 必须如实说出来，而不是编一个原因，也不是把它当成功。
 */
export function humanError(e: unknown): string {
  const extracted = extractReason(e, 0);
  if (extracted !== null) return extracted;

  if (e === null) return `${NO_REASON}（后端只返回了 null）`;
  if (e === undefined) return `${NO_REASON}（后端只返回了 undefined）`;
  if (typeof e === "string") return `${NO_REASON}（后端只返回了空字符串）`;
  if (Array.isArray(e)) return `${NO_REASON}（后端只返回了一个空数组）`;
  if (isObjectLike(e)) {
    try {
      const json = JSON.stringify(e);
      if (json && json !== "{}" && json !== "[]") {
        return `后端返回了无法识别的错误：${clip(json)}`;
      }
    } catch {
      // 循环引用 / BigInt：下面统一按「读不出来」处理。
    }
    return `${NO_REASON}（后端只返回了一个空对象）`;
  }
  // 剩下的原始类型：说出来，但别让它看起来像一句正常的原因。
  return `${NO_REASON}（后端只返回了 ${String(e)}）`;
}

// ---------------------------------------------------------------------------
// 下一步动作
// ---------------------------------------------------------------------------

/**
 * 动作的**稳定标识**。同一个动作在多个线索下命中时只出现一次 ——
 * 「换节点 + 换节点再试」这种重复会把真正有用的那一条挤掉。
 */
type StepKey =
  | "reinstall-helper"
  | "change-node"
  | "install-ca"
  | "fix-port"
  | "check-core"
  | "open-logs";

/** 动作文案。**只写本应用真的有的入口**（对照 `App.tsx` 的导航项）。 */
const STEP_TEXT: Record<StepKey, string> = {
  "reinstall-helper":
    "重装助手：「设置 → 系统与助手」里点「重新安装助手」（会重新申请一次系统授权）",
  "change-node": "换一个节点再试：「节点」页可以先测延迟再选",
  "install-ca": "按顺序做：装入根证书 → 应用（起/停代理）→ 重连核心（都在「意图过滤 → MITM」）",
  "fix-port": "换一个本地端口（「设置 → 端口」），或先关掉占用该端口的程序",
  "check-core": "确认核心路径：「设置 → 核心与数据更新」",
  "open-logs": "看日志：「日志」页有核心输出的最后几行",
};

/**
 * 线索 → 动作。**线索来自后端自己写的文案**（命令返回的 Err(String) 就是给人看的
 * 中文），命中即给对应动作；命中不了就只剩 `open-logs`。
 *
 * 顺序 = 优先级：更具体的失败排前面（端口占用比「连接失败」更具体）。
 */
const STEP_RULES: ReadonlyArray<{ readonly match: RegExp; readonly steps: readonly StepKey[] }> = [
  { match: /helper|助手/i, steps: ["reinstall-helper"] },
  { match: /证书|钥匙串|keychain|信任锚|根证书|mitm/i, steps: ["install-ca"] },
  { match: /端口|占用|address already in use|eaddrinuse/i, steps: ["fix-port"] },
  { match: /权限|特权|授权|permission|not permitted|denied/i, steps: ["reinstall-helper"] },
  { match: /核心|core|xray|可执行文件/i, steps: ["check-core"] },
  { match: /节点|node|订阅|机场|超时|timeout|无响应/i, steps: ["change-node"] },
  { match: /连接|connect|握手|handshake|tls|econn|refused|reset|网络|network|dns|代理/i, steps: ["change-node"] },
];

/** 每个失败里**总是**成立的那一条：日志一直在，读它不需要任何前提。 */
const ALWAYS: readonly StepKey[] = ["open-logs"];

/** 按线索算出动作键（顺序 = 优先级，`open-logs` 永远兜底）。 */
function stepKeys(text: string): StepKey[] {
  const keys: StepKey[] = [];
  for (const rule of STEP_RULES) {
    if (!rule.match.test(text)) continue;
    for (const k of rule.steps) if (!keys.includes(k)) keys.push(k);
  }
  for (const k of ALWAYS) if (!keys.includes(k)) keys.push(k);
  return keys;
}

/**
 * 从失败文案推出「下一步可以做什么」。**最多 3 条**，去重、按优先级。
 *
 * 不变量（测试锁着）：
 * 1. 结果**非空**：任何失败至少给「看日志」；
 * 2. 线索命中时给**该线索的动作**（助手 ⇒ 重装助手；节点/连接 ⇒ 换节点）；
 * 3. 线索命中不了时**不编**具体归因 —— 只给「看日志」。
 */
export function nextSteps(text: string): string[] {
  return stepKeys(text).slice(0, 3).map((k) => STEP_TEXT[k]);
}

/** 界面上真的可以点的动作。`open-logs` 永远在最后（它没有任何前提）。 */
export type FailureActionId = "reinstall-helper" | "change-node" | "open-logs";

export interface FailureAction {
  id: FailureActionId;
  label: string;
}

const ACTION_LABEL: Record<FailureActionId, string> = {
  // 「去」字是刻意的：重装助手要管理员授权，**绝不静默自动执行**
  // （Settings.tsx 的同名按钮也是用户点一下才走 `install_helper`）。
  "reinstall-helper": "去重装助手",
  "change-node": "去换一个节点",
  "open-logs": "查看日志",
};

/**
 * 把 `nextSteps` 的线索转成**可点击的动作**（U3）。
 *
 * 为什么不能只有文字建议：连接失败横幅原来一个按钮都没有，用户看完一段红字
 * 还得自己去侧栏找「节点」；而「重装助手」的入口只存在于设置页、且只在
 * `version_check === "mismatch"` 时才出现 —— 也就是**最需要它的那一刻不在**。
 */
export function failureActions(text: string): FailureAction[] {
  const keys = stepKeys(text);
  const out: FailureAction[] = [];
  for (const id of ["reinstall-helper", "change-node"] as const) {
    if (keys.includes(id)) out.push({ id, label: ACTION_LABEL[id] });
  }
  out.push({ id: "open-logs", label: ACTION_LABEL["open-logs"] });
  return out;
}

/**
 * 后端文案里的 `**强调**`（`supervisor.rs:459-462` 等）不是 Markdown 渲染器能
 * 吃掉的东西：JSX 里它就是一个普通字符串，用户看到的是**星号本身**。
 * 这里只去掉成对记号，换行交给 `.banner__reason { white-space: pre-wrap }`。
 */
export function stripMarkup(text: string): string {
  return text.replace(/\*\*(.+?)\*\*/g, "$1");
}

/**
 * 把后端散文压成**一行**：去掉 `**` 记号，并把换行与连续空白压成单个空格。
 *
 * # 为什么需要它（task-15）
 *
 * `stripMarkup` 保留换行是对的 —— 横幅有 `white-space: pre-wrap`，多行可读。
 * 但同一段文本还会经 `topbarStatus.appStatus()` 进两个**不能保留换行**的地方：
 *
 * * `App.tsx` 顶栏的 `title={status.detail}`（悬停 tooltip）；
 * * 同一个 `detail` 还进 `role="status"` 的 `.sr-only` live region（**读屏用户
 *   唯一的通道**）。
 *
 * 那里换行要么被折叠得莫名其妙、要么被逐字念出来，而 `**` 会**被读屏逐字读成
 * 「星号 星号 …」** —— 比视觉上的乱码更糟：听的人拿不到任何「这是记号」的线索。
 * 所以这两个载体统一走本函数：记号去掉、换行压成空格。
 */
export function plainOneLine(text: string): string {
  return stripMarkup(text)
    .replace(/\s*\n+\s*/g, " ")
    .replace(/\s{2,}/g, " ")
    .trim();
}

/**
 * 「门禁未过」这一刻，后端文案可能叫用户「点『断开』」，而按钮写的是「连接」。
 *
 * 后端文案归 `supervisor.rs`，前端**不改它**（改了就是两处真源）；但同一屏里
 * 界面必须说清**按钮现在叫什么**。判据只有一条真实字段：`running`。
 * 门禁失败发生在提交默认路由之前 ⇒ 核心没在跑 ⇒ 顶栏按钮是「连接」。
 *
 * 返回 `null` = 不需要补充（正在跑，或文案根本没提「断开」）。
 */
export function buttonNameNote(text: string, running: boolean): string | null {
  if (running || !text.includes("断开")) return null;
  return (
    "这条错误发生时核心没有在运行（失败发生在接管默认路由之前）：" +
    "顶栏右上角那个按钮现在写的是「连接」，不是「断开」——" +
    "文案里的「断开」只适用于已经在跑的隧道。"
  );
}

/** 一次失败的完整说法：人话原因 + 下一步。 */
export function failureAdvice(e: unknown): FailureAdvice {
  const text = humanError(e);
  return { text, steps: nextSteps(text) };
}
