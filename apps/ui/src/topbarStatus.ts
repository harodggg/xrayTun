/**
 * 应用状态语义的**唯一真源**（task-45 建立，task-47 合并）。
 *
 * > 文件名沿用任务卡里的 `topbarStatus.ts`；但这里的函数从 task-47 起同时驱动
 * > **顶栏状态线**与**仪表盘状态区**两个界面 —— 名字里的 `topbar` 是历史，不要再
 * > 拿它当「只管顶栏」的依据。
 *
 * # 为什么要有这个文件
 *
 * 这个缺陷发生过两次，根因是同一个：**同一件事在两处各写一份判断**。
 *
 * 1. **task-45**：顶栏底边那条 2px 的线被写死成 `var(--ok)`，于是直连 / 系统代理 /
 *    未连接 / 恢复中 / 已退回直连**全都是绿的**。而绿色在这套配色里 = 已受保护 ——
 *    直连模式下等于谎报「你受保护了」（实际毫无代理）。
 * 2. **task-47**：线修好之后，`Dashboard` 的状态词**仍然不看 `mode`** ——
 *    系统代理模式下它走的是 `!routes_committed` 分支，写出「**隧道已建立**」
 *    「默认路由尚未接管」。而系统代理模式**根本没有隧道、也不接管路由**。
 *    结果：**蓝线 + 「隧道已建立」**，比原来全绿更让人困惑。
 *
 * 所以规矩是：**判据只写在这里**。顶栏、状态区、圆点都只是它的三种呈现。
 * 任何地方再抄一份 `if (routes_committed)` 都是把同一个缺陷种回去。
 *
 * # 判据只来自后端真实字段
 *
 * `settings.mode` / `runtime.running` / `runtime.routes_committed` /
 * `runtime.last_error` / `core.path` / `runtime.recovery`（经既有的纯函数
 * `recoveryView` 翻译成三段）。**不允许前端按时间、次数或 notice 文案猜。**
 */

import type { RecoveryView } from "./ipc";
import { nextSteps } from "./failure";
import type { ProxyMode } from "./types";

/**
 * 五种色调。绿色**只属于** `on`（整机受保护）。
 *
 * | tone      | 含义                             | 颜色           |
 * |-----------|----------------------------------|----------------|
 * | `on`      | 整机受保护（TUN 在跑）           | `--ok` 绿      |
 * | `partial` | 本地入口就绪但**系统代理未设置** | `--accent` 蓝  |
 * | `off`     | 没有代理覆盖（直连 / 未运行）    | `--status-off` |
 * | `busy`    | 正在自愈 / 隧道建好但路由没接管  | `--warn` 琥珀  |
 * | `failed`  | 代理承诺已破（退回直连 / 报错）  | `--danger` 红  |
 */
export type StatusTone = "on" | "partial" | "off" | "busy" | "failed";

/** tone → 顶栏底边那条状态线的修饰类名。 */
export const TOPBAR_TONE_CLASS: Record<StatusTone, string> = {
  on: "topbar--on",
  partial: "topbar--partial",
  off: "topbar--off",
  busy: "topbar--busy",
  failed: "topbar--failed",
};

/**
 * tone → 小圆点的修饰类名。
 *
 * 顶栏的线和点**必须同色**：它们相距几像素、说的是同一件事。
 * 仪表盘的状态圆点用同一个映射，三处颜色天然一致。
 */
export const DOT_TONE_CLASS: Record<StatusTone, string> = {
  on: "dot--on",
  partial: "dot--partial",
  off: "dot--off",
  busy: "dot--warn",
  failed: "dot--failed",
};

/** tone → 仪表盘状态词的修饰类名（决定 `.dash__state-label` 的颜色）。 */
export const DASH_TONE_CLASS: Record<StatusTone, string> = {
  on: "dash__state--on",
  partial: "dash__state--partial",
  off: "dash__state--off",
  busy: "dash__state--busy",
  failed: "dash__state--failed",
};

export interface StatusInput {
  mode: ProxyMode;
  running: boolean;
  routesCommitted: boolean;
  lastError: string | null;
  /** `core.path`：为 `null` 表示找不到核心可执行文件。 */
  corePath: string | null;
  /** 由 `recoveryView(recovery, running)` 得到，**不要**在这里重新解释原始字段。 */
  recovery: RecoveryView;
  /**
   * 本地 SOCKS 入站端口（`settings.socks_port`）。
   *
   * 用于「系统代理」模式的文案：**这个模式只提供本地入口，从不修改系统代理设置**
   * （`docs/07-roadmap-and-risks.md` 里「系统代理模式真正生效」仍是未勾选项；
   * 全仓对系统代理的写入 0 命中）。既然要说「指向哪个端口」，就得把真实端口写对。
   * 快照缺席时为 `null` —— **那就一个数字都不写**（不编）。
   */
  socksPort: number | null;
  /** 本地 HTTP 入站端口（`settings.http_port`）；缺席时同样不写数字。 */
  httpPort: number | null;
  /**
   * 快照**是否已经拿到**（`store.snapshot !== null`）。
   *
   * 为什么要单独一个字段：`corePath` 缺席有两种**完全相反**的来源 ——
   * ① 快照还没回来（**不知道**）；② 快照回来了、`core.path === null`
   * （**确实找不到核心**）。旧写法把 `snapshot?.core.path ?? null` 压成同一个
   * `null`，于是冷启动那一瞬顶栏会红着说「未找到核心」，用户看到的是故障、
   * 其实只是一个还没回来的 IPC。默认 `true` = 老调用方语义不变。
   */
  snapshotLoaded?: boolean;
}

export interface AppStatus {
  tone: StatusTone;
  /** 状态区的大字（仪表盘）。 */
  label: string;
  /** 状态词的补充说明（仪表盘）；`null` = 不需要补充。 */
  sub: string | null;
  /** 一句话说明当前状态（顶栏的 `title` 与屏幕阅读器文本）。 */
  detail: string;
}

/**
 * 把后端字段翻译成 ONE 状态。**顶栏与状态区都必须走这里。**
 *
 * 优先级（顺序即语义，改动前先想清楚）：
 *
 * 1. **找不到核心** —— 连启动都做不到，最靠前（原 `Dashboard` 的顺序）。
 * 2. **正在自愈** —— 此刻既不是「受保护」也不是「未连接」，说成任一个都会
 *    让用户去点「连接」和看门狗抢。
 * 3. **已退回直连** —— `mode` 此时**仍然是 `"tun"`**，所以必须排在所有按
 *    `mode` 判断的分支之前；否则「流量已在裸奔」会被画成受保护。
 * 4. **直连模式** —— 用户**主动选的**，中性报告。**不能说成「未连接」**：
 *    那会让人以为出了问题，而直连本来就是「有意不接管」。
 * 5. **核心没在跑**（有错 → 故障；无错 → 空闲）。
 * 6. 之后才是「在跑」的三种：TUN 未接管路由 → 中间态；TUN → 受保护；
 *    系统代理 → 部分覆盖。
 *
 * `routes_committed` **只对 TUN 生效**：它是 TUN 两阶段启动的闸门
 * （`supervisor.rs` 里提交路由那段在 `if mode == Tun` 分支内）；系统代理不接管
 * 路由，拿它压系统代理会把「部分覆盖正常工作中」误报成「流量还没走代理」。
 */
export function appStatus(input: StatusInput): AppStatus {
  const base = baseStatus(input);
  const { recovery } = input;
  /**
   * task-68：`degraded`（探测失败、但看门狗还没开始重建）**不改状态词、不改色调** ——
   * 这就是 task-60 的 tone 决定：`running` 仍为真、隧道仍在，「1 次失败」不是状态变化，
   * 把它渲染成 `busy`/`failed` 等于把**设计内**的过程说成故障（另一种假陈述）。
   *
   * 这里只做一件事：把 `recoveryView` 已经写好的自救句接进 `sub` 与 `detail`。
   * **`detail` 是顶栏 `.sr-only` + `role="status"` 的文本** —— 不接进来，
   * 读屏用户就收不到「整机断网时先点『断开』」这句话，而那正是这次修复的全部内容。
   *
   * 例外：基础状态是 `failed`（例如根本没找到核心）时**不覆盖** ——
   * 自救提示不能把一个更严重的结论盖成轻的。
   */
  if (recovery.phase === "degraded" && recovery.hint && base.tone !== "failed") {
    return {
      ...base,
      sub: base.sub ? `${base.sub}；${recovery.hint}` : recovery.hint,
      detail: `${base.detail} —— ${recovery.hint}`,
    };
  }
  return base;
}

function baseStatus(input: StatusInput): AppStatus {
  const { mode, running, routesCommitted, lastError, corePath, recovery, socksPort, httpPort } =
    input;

  // 0) 快照还没到 ⇒ **什么都不知道**。既不能说「未找到核心」（那是故障），
  //    更不能是绿色（那是「已受保护」）。「不知道」只能显示成「不知道」。
  if (input.snapshotLoaded === false) {
    return {
      tone: "off",
      label: "正在读取状态…",
      sub: null,
      detail: "还没有拿到后端状态 —— 现在无法判断流量是否受保护（这不等于「未找到核心」）",
    };
  }

  // 1) 核心可执行文件都不在 —— 没有任何「受保护」的可能。
  if (corePath === null) {
    return {
      tone: "failed",
      label: "未找到核心",
      sub: "缺少 Xray 可执行文件",
      detail: "未找到核心 —— 缺少 Xray 可执行文件",
    };
  }

  // 2) 看门狗正在重建隧道。文案永远带「恢复」，不退化成「未连接」。
  if (recovery.phase === "recovering") {
    const label = recovery.text ?? "正在自动恢复";
    return {
      tone: "busy",
      label,
      sub: "看门狗正在重建隧道，不需要手动点「连接」（点了会打断它）",
      detail: `${label} —— 看门狗在重建隧道，不需要手动连接`,
    };
  }

  // 3) 自动恢复失败、已退回直连。**`mode` 此时仍是 `"tun"`** ——
  //    按模式判断会把它当成绿/蓝，而那正是最严重的一种假陈述。
  if (recovery.phase === "failed") {
    return {
      tone: "failed",
      label: "自动恢复失败",
      sub: "已退回直连：网络可用，但流量不再走代理 —— 可手动重连，或换一个节点",
      detail: "自动恢复失败 —— 已退回直连，流量不再走代理",
    };
  }

  // 4) 直连是**有意为之**，不是故障。说「未连接」会让人以为坏了。
  if (mode === "direct") {
    return {
      tone: "off",
      label: "直连模式",
      sub: "不接管任何流量（有意为之，不是故障）",
      detail: "直连模式 —— 不接管任何流量",
    };
  }

  // 5a) 上次运行出错且现在没在跑 —— 如实说故障，不装成普通的「未连接」。
  //
  //     并且**同屏给出下一步**：连接失败时用户看到的不该只是一个错误码。
  //     动作由 `failure.ts::nextSteps` 按后端自己的文案推（换节点 / 重装助手 /
  //     看日志…），这里只负责把它接到状态里。
  if (lastError !== null && !running) {
    const steps = nextSteps(lastError);
    const advice = steps.length > 0 ? `下一步：${steps.join("；")}` : null;
    return {
      tone: "failed",
      label: "核心未运行",
      sub: advice ? `${lastError} —— ${advice}` : lastError,
      detail: advice ? `核心未运行 —— ${lastError}；${advice}` : `核心未运行 —— ${lastError}`,
    };
  }

  // 5b) 核心没在跑（用户还没点连接）→ 空闲，中性。
  if (!running) {
    return {
      tone: "off",
      label: "未连接",
      sub: "核心没有运行",
      detail: "未连接 —— 核心没有运行",
    };
  }

  // 6) 以下都是「核心在跑」。TUN 的承诺是「接管全部流量」，
  //    两阶段启动里隧道先建好、默认路由还没接管 —— 此时不能说「已连接」。
  if (mode === "tun") {
    if (!routesCommitted) {
      return {
        tone: "busy",
        label: "隧道已建立",
        sub: "默认路由尚未接管，流量还没有走代理",
        detail: "隧道已建立 —— 默认路由尚未接管，流量还没有走代理",
      };
    }
    // 只有这里才是名副其实的「受保护」。
    return {
      tone: "on",
      label: "已连接",
      sub: null,
      detail: "TUN 模式运行中 —— 整机流量受保护",
    };
  }

  // 7) 「系统代理」模式：**应用只提供本地入站入口，从不修改系统代理设置。**
  //
  //    这里原来写的是「系统代理已启用」+「只有读取系统代理设置的应用走代理」。
  //    后一句是真的，**前一句比事实强**：`docs/07-roadmap-and-risks.md` 里
  //    「系统代理模式真正生效」仍是未勾选项，明写「用户需要手动把浏览器/系统代理
  //    指向 127.0.0.1:…」；全仓对系统代理的写入（`setwebproxy` / `scutil` /
  //    `SCDynamicStore`）**0 命中**。用户看到「已启用」就会以为浏览器已经在走代理。
  //
  //    所以：label 只说**已经成立**的事（本地入口就绪），把「需要你手动指向」
  //    第一次说给用户，并带上真实端口（读不到端口就一个数字都不写）。
  //    **仍然不许出现「隧道」** —— 这个模式没有隧道，也不接管路由。
  const socks = socksPort !== null ? `127.0.0.1:${socksPort}` : null;
  const http = httpPort !== null ? `127.0.0.1:${httpPort}` : null;
  const entry = socks
    ? `本地 SOCKS 入口 ${socks}${http ? ` 与 HTTP 入口 ${http}` : ""} 已就绪`
    : "本地代理入口已就绪";
  const pointTo = socks ? `指向 ${socks}${http ? `（HTTP ${http}）` : ""}` : "指向它的本地端口";
  return {
    tone: "partial",
    label: "本地代理入口已就绪",
    sub: `系统代理未被本应用修改：需要手动把浏览器或系统代理${pointTo}。`,
    detail: `${entry} —— 系统代理未被本应用修改，需要手动${pointTo}`,
  };
}

/**
 * 顶栏那条「本应用不设系统代理」徽章的文字；`null` = **不显示**。
 *
 * task-120 改名：原来写的是「**未设**系统代理 · 需指向 …」—— 那是**对这台机器上
 * 系统代理现状的断言**，而这个 App **从来不读**系统代理设置（全仓
 * `setwebproxy` / `getwebproxy` / `scutil --proxy` / `SCDynamicStore` 0 命中）。
 * 用户照这句话手动设好代理之后，徽章仍然写着「未设系统代理」—— 界面就地变成假话。
 * 现在只说**本应用这一侧**可核实的事实（它不设），用户侧的状态不猜。
 *
 * # 为什么只在「核心运行中 + 系统代理模式」显示（task-72）
 *
 * `ProxyMode::SystemProxy` 是 `#[default]`，所以**新用户一打开就是它** ——
 * 而那时并没有代理入口需要指向，徽章只是噪音。等真的连上、本地入站端口在监听了，
 * 它才是一条**真事实**，而且正是那一刻用户需要它（要手动把浏览器/系统代理指过来）。
 *
 * 注意区分：**徽章是按条件出现的载体，信息本身不随条件消失** ——
 * 仪表盘的 `sub` 与 live region 里的同一句话不受此函数影响（它们有自己的显示条件，
 * 见 `appStatus` 的第 7 个分支）。
 */
export function systemProxyBadge(
  mode: ProxyMode,
  running: boolean,
  socksPort: number | null,
): string | null {
  if (!running || mode !== "system_proxy") return null;
  return socksPort !== null
    ? `本应用不设系统代理 · 需手动指向 127.0.0.1:${socksPort}`
    : "本应用不设系统代理 · 需手动指向本地端口";
}

/**
 * 顶栏「连接/断开」是否禁用。
 *
 * 抽出来是为了让「**恢复期间不得可点**」这条不变量有单测。task-60 ② 把它钉在
 * 仪表盘那颗按钮上；task-72 把仪表盘那颗按钮收敛掉了（它与顶栏是同一个命令的
 * 等价按钮），**不变量必须跟着搬到顶栏，不能随按钮一起删掉** ——
 * 否则用户又能在看门狗重建时点「连接」把它打断。
 */
export function runButtonDisabled(
  rv: RecoveryView,
  opts: { runBusy: boolean; mode: ProxyMode },
): boolean {
  return opts.runBusy || opts.mode === "direct" || rv.button === "recovering";
}
