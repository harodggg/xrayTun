/**
 * 仪表盘。
 *
 * # 这一版的设计取舍
 *
 * 旧版把「现在通不通」拆在了三个地方：一张写着 pid 的卡片、顶栏的绿点、
 * 以及一条黄色横幅（「路由尚未接管」）。用户得自己把三处拼成一个结论 ——
 * 而规范第 2 条明确要求「连接成功必须精确表达」。
 *
 * 所以这里改成**一个状态区**：左侧圆点 + 状态词，右侧是唯一的主操作按钮，
 * 下面一行把「模式 · 路由 · 节点 · 延迟」串成一句话。那三个信息源合并成一处，
 * 中间态（隧道已建、路由未接管）作为状态词本身出现，而不是又一条横幅。
 *
 * 另外两处收敛：
 * * **横幅只留最要紧的一条。** 旧版最多会同时堆 5 条 + 底部一条 last_error，
 *   全部同权重，「流量还没走代理」和「helper 没装」看起来一样急。
 *   现在按严重度排序只显示第一条，其余折进一行「还有 N 条」。
 * * **流量与操作降低层级。** 速率是「看」的，不是「做」的，所以做成一行
 *   紧凑文本而不是两张占满的卡片；进程 pid、配置路径这类诊断信息收进
 *   「环境自检」折叠区 —— 出问题时才需要。
 */

import { useState } from "react";

import { api, recoveryView } from "../ipc";
import { InlineConfirm } from "../InlineConfirm";
// 状态语义的唯一真源：顶栏的线与这里的状态词必须同源（task-47）。
import { appStatus, DASH_TONE_CLASS, DOT_TONE_CLASS } from "../topbarStatus";
import { useStore } from "../store";
import type { AppSnapshot, UpdateStatus } from "../types";
import {
  formatBytes,
  formatRate,
  formatTimestamp,
  latencyTier,
  MODE_LABEL,
  PRESET_LABEL,
} from "../types";

/** 一条横幅。`tone` 决定配色，`rank` 只用于排序（越小越急）。 */
export interface Notice {
  key: string;
  tone: "error" | "warn" | "info";
  icon: string;
  text: React.ReactNode;
  rank: number;
  /** 需要用户立刻动手修的情况，附一个动作。 */
  action?: {
    label: string;
    run: () => void;
    /**
     * **破坏性动作**（改系统网络配置、删数据）的二次确认问句。
     *
     * 问句必须写清**真实的、能从代码确认的后果**（红线：编不出来的后果就不要写）；
     * 只是导航（「去处理」）之类的动作**不加**确认 —— 那会变成纯摩擦。
     */
    confirm?: { question: string; confirmLabel: string };
  };
}

/**
 * 横幅右侧的动作按钮。带 `confirm` 的走既有 `InlineConfirm`，其余直接执行。
 *
 * 抽成独立组件是因为「未确认时后端 API 不得被调用」这条断言需要**渲染**它 ——
 * 为此渲染整个仪表盘要拉一整套 store 与 Tauri 桥接，那会让人不愿意跑这条测试。
 */
export function NoticeAction({ action }: { action: NonNullable<Notice["action"]> }) {
  if (!action.confirm) {
    return (
      <button className="btn" onClick={action.run}>
        {action.label}
      </button>
    );
  }
  return (
    <InlineConfirm
      label={action.label}
      className="btn"
      question={action.confirm.question}
      confirmLabel={action.confirm.confirmLabel}
      onConfirm={action.run}
    />
  );
}

/**
 * 「有没有新版本」这条提示该说什么（task-189）。
 *
 * # 为什么要有这条（用户的原始需求）
 *
 * 用户要的是「**自动检测最新版本**」。`task-188` 让后端启动查一次 + 每 6h 复查，
 * 但**结果只出现在设置页的「核心与数据更新」分节里** —— 不打开设置页就永远看不到，
 * 自动检测等于白做。所以这里把它提到**仪表盘状态区**（默认落地页）。
 *
 * # 判据：**只用客户端专属字段**（task-193；`task-189` 建了这条提示，`task-190` 补了组合态）
 *
 * | 条件 | 结论 | 文案要能看出 |
 * |---|---|---|
 * | `check_error_app` + `app_update_available` + `latest_app` | `available`（带 `staleReason`） | 「有新版本 vX.Y.Z」**和**「最近一次**客户端**更新检查没成功」**同时**说 |
 * | `check_error_app`（其余） | `failed` | 「**客户端**更新检查没查到」+ 原因 —— **绝不许**写成「已是最新」 |
 * | `app_update_available === true` | `available` | 「有新版本 vX.Y.Z」 |
 * | `latest_app` 有值 + 不可更新 | `latest` | 「**客户端**已是最新（vX.Y.Z）」 |
 * | `checked_at_app === null` | `unknown` | 「还没检查过**客户端**更新」——**不假装知道结果** |
 * | 查过但 `latest_app === null` | `unknown` | 「客户端检查没拿到版本信息」——**不**等同于「已是最新」 |
 *
 * # 为什么判据从 `check_error` 换成 `check_error_app`（task-193）
 *
 * `check_error` / `checked_at` 是**客户端 / 核心 / geo 三类检查共用**的合并字段
 * （`state.rs:281-297` 明写「仪表盘判断客户端有没有新版**不要**用它」；写入者见
 * `version_check.rs::apply_app_check_result` 与 `_core_geo_check_result`）。用它下客户端结论
 * 会产生两类**真实的错报**：
 * * **over-claim 方向**（`task-190` 修的就是这条）：`check_error` 非空 ⇒ 说「不知道有没有
 *   新版本」，而核心/geo 失败时客户端其实可能**已确证是最新** ⇒ 对已知事实装瞎；
 * * **反向**（`task-189` 的红线）：绝不能把「客户端没查到」说成「已是最新」。
 * ⇒ 客户端结论只由 `check_error_app` / `checked_at_app` / `latest_app` / `app_update_available`
 *   决定；`check_error`（合并）**不得**改变它 —— 有一条对五个状态逐一对比的测试钉住。
 *
 * # `task-190` 的组合态为什么仍然成立
 *
 * 客户端检查失败时后端**不清** `latest_app`（`version_check.rs::apply_app_check_result`：`Err`
 * 分支只写 `check_error_app`/`check_error`，`Ok` 分支才写 `latest_app`），而
 * `app_update_available` 是 `update_status_with` 每个快照按 `latest_app` 与当前版本重算的
 * ⇒「查到过有新版 + 这次**客户端**复查失败」真实可达：既要说「有新版本 vX」，又要说清
 * 「版本号是上次成功查到的、可能已经不是最新」。**红线**：这个组合态绝不许说成「已是最新」。
 * 措辞只说「上次**成功**查到的」（`latest_app` 唯一的写入路径），不断言「上一次检查」。
 *
 * ⚠️ 本提示**只谈客户端**（`task-193` 由 Lead 定：**一条事实一处陈述**）。核心/geo 的失败
 * 在设置页「核心与数据更新」分节有展示位，这里**不**复述 —— 复述就是把两类失败重新混成
 * 一句，正是本卡要修的东西。
 */
export type UpdateNotice =
  | {
      kind: "available";
      version: string;
      /**
       * task-190 引入、task-193 改绑到**客户端专属**字段：这个是「上次成功检查看到的版本」，
       * 而最近一次**客户端**检查没成功（`= check_error_app`）。
       *
       * 口径：`latest_app.version` 是那次**成功**检查的事实（`version_check.rs::
       * apply_app_check_result` 的 `Err` 分支只写 `check_error_app`、**不清** `latest_app`），
       * 所以「有新版本 vX」这句话仍然成立；但复查失败意味着**可能已经不是最新**了 ——
       * 所以必须同时带上这个限定语。有值时渲染成
       * 「（最近一次客户端更新检查没成功；版本号是上次成功查到的）」，`title` 里给原始原因。
       */
      staleReason?: string;
    }
  | { kind: "latest"; version: string }
  | { kind: "failed"; reason: string }
  | { kind: "unknown"; reason: string };

export function updateNotice(u: UpdateStatus): UpdateNotice {
  // **只用客户端专属字段**（task-193）。`check_error` / `checked_at` 是客户端/核心/geo
  // **共用**的合并字段（`state.rs` 明写不要用它下客户端结论）⇒ 用它会把
  // 「核心/geo 失败 + 客户端已确证是最新」错报成「不知道有没有新版本」（under-claim，
  // 也就是 task-190 修掉的那类「对已知事实装瞎」的反方向）。理由与锚点见上面的注释。
  if (u.check_error_app) {
    // **失败优先 + 不藏已知信息**（task-190）：曾经成功查到过有新版时，后端会保留
    // `latest_app`（`Err` 分支不清它），而 `app_update_available` 每个快照按它重算
    // ⇒ 这个组合态要**同时**说出两件事，否则会把用户最该看到的那条提示一起藏掉。
    if (u.app_update_available && u.latest_app) {
      return {
        kind: "available",
        version: u.latest_app.version,
        staleReason: u.check_error_app,
      };
    }
    // 客户端自己没查到、也没有已知新版 ⇒ failed：不许说成「已是最新」，也不许凭空说有新版。
    return { kind: "failed", reason: u.check_error_app };
  }
  if (u.app_update_available && u.latest_app) {
    return { kind: "available", version: u.latest_app.version };
  }
  // `checked_at_app`（客户端专属）而不是 `checked_at`（三路共用）：核心/geo 查过不代表
  // 客户端查过，用后者会把「客户端还没查过」说成「查过了」（进而可能说成「已是最新」）。
  if (u.checked_at_app === null) return { kind: "unknown", reason: "还没检查过客户端更新" };
  if (!u.latest_app) return { kind: "unknown", reason: "客户端更新检查没拿到版本信息" };
  return { kind: "latest", version: u.latest_app.version };
}

export default function Dashboard({
  onNavigate,
}: {
  /** `target` 是目标分节（如 `set-helper`）：设置页是两级结构，带目标才会落到正确的分类。 */
  onNavigate: (view: string, target?: string) => void;
}) {
  const { snapshot, busy, run, probing, recovery } = useStore();
  const [showAllNotices, setShowAllNotices] = useState(false);
  if (!snapshot) return <div className="empty">正在加载…</div>;

  const { runtime, helper, core, nodes, settings, traffic, latency } = snapshot;
  const selected = nodes.find((n) => n.id === settings.selected_node) ?? null;
  const selectedLatency = selected ? latency[selected.id] : undefined;
  const rtt = selectedLatency?.server_rtt_ms ?? null;
  const connected = runtime.running;
  /**
   * 自动恢复（task-22）：看门狗在自愈时**不能说成「未连接」** —— 那会让用户以为
   * 网络断了、去点「连接」，正好和看门狗抢。三态由结构化状态（`runtime.recovery`）
   * 驱动，**不解析 notice 文案**。
   */
  const rv = recoveryView(recovery, connected);

  // ---- 状态词：**唯一真源**（task-47）----
  //
  // 这里以前自己抄了一份判断：不看 `mode`，只按 `routes_committed` 分流。
  // 后果是系统代理模式下写出「**隧道已建立**」「默认路由尚未接管」——
  // 而系统代理根本没有隧道、也不接管路由；同一屏的顶栏线却是蓝的（部分覆盖），
  // 文字与线互相矛盾。根因就是「同一事实两处陈述」。
  //
  // 现在顶栏的线/点与这里的词/说明/圆点**全部来自 `appStatus`**。
  // 要改判据请改 `apps/ui/src/topbarStatus.ts` —— **不要在这里再抄一份**。
  const status = appStatus({
    mode: settings.mode,
    running: connected,
    routesCommitted: runtime.routes_committed,
    lastError: runtime.last_error,
    corePath: core.path,
    recovery: rv,
    // 端口只用于「系统代理」模式的文案（那半句要说清指向哪个端口）。
    socksPort: settings.socks_port,
    httpPort: settings.http_port,
  });
  const state = {
    label: status.label,
    sub: status.sub,
    toneClass: DASH_TONE_CLASS[status.tone],
    dotClass: DOT_TONE_CLASS[status.tone],
  };

  const notices = collectNotices(snapshot, run, onNavigate);
  const [primary, ...rest] = notices;
  const visible = showAllNotices ? notices : primary ? [primary] : [];

  return (
    <div className="dash">
      {/* ---------------------------------------------------------- 状态区 */}
      <section className="dash__status">
        <div className={`dash__state ${state.toneClass}`}>
          <span className={`dot ${state.dotClass}`} />
          <span className="dash__state-label">{state.label}</span>
          {connected && selected && (
            <>
              <span className="dash__sep">·</span>
              <span className="dash__state-node">{selected.name}</span>
              {/* task-120：**延迟数字不能替「这个节点能不能用」背书。**
                  `ProbeResult` 里有 `available` 与 `server_rtt_ms` 两个独立字段，
                  后端明确保留「不可用但量得到距离」这一态
                  （`xray/probe.rs:296-298`：`available=false` 时仍然写回 rtt）。
                  节点页早就按这个口径做了中性色（`Nodes.tsx:257`
                  `available === false ? "unknown" : latencyTier(...)`），
                  而这里原来无条件按 `latencyTier(rtt)` 上色 ——
                  一个刚探测失败的节点会在这里显示成绿色的「53 ms」。 */}
              {selectedLatency?.available === false ? (
                <span
                  className="badge badge--unknown"
                  title={
                    rtt !== null
                      ? `最近一次探测：经该节点取不到数据（${rtt} ms 只是本地到服务器的 TCP 距离，不代表能用）`
                      : "最近一次探测：经该节点取不到数据"
                  }
                >
                  {rtt !== null ? `${rtt} ms · 不可用` : "不可用"}
                </span>
              ) : (
                rtt !== null && <span className={`badge badge--${latencyTier(rtt)}`}>{rtt} ms</span>
              )}
            </>
          )}
        </div>

        <div className="dash__meta">
          {MODE_LABEL[settings.mode]}
          <span className="dash__sep">·</span>
          {PRESET_LABEL[settings.routing_preset]}
          {runtime.tun_interface && (
            <>
              <span className="dash__sep">·</span>
              {runtime.tun_interface}
            </>
          )}
          {connected && runtime.started_at_unix && (
            <>
              <span className="dash__sep">·</span>
              已运行 {elapsed(runtime.started_at_unix)}
            </>
          )}
        </div>

        {/* `state.sub` 现在同时承载三件事：系统代理模式的「需要手动指向」、
            以及（task-68）探测失败窗口的自救提示 —— 两者都由 `appStatus` 折进来，
            这样顶栏 `title`、live region 与这里**同一份文本、同一个真源**。 */}
        {state.sub && <div className="dash__state-sub">{state.sub}</div>}

        {/* task-189：把「有没有新版本」摆在**默认落地页**的状态区里 ——
            用户不打开设置页也能看到（那是这一卡的全部意义）。
            刻意**不**走 `collectNotices`：那套只显示最急的一条，其余折进
            「还有 N 条提示」，新版本提示会被压在下面而看不见。
            点一下走既有的带意图跳转（`task-48`）：`onNavigate("settings", "set-update")`。 */}
        {(() => {
          const n = updateNotice(snapshot.update);
          const stale = n.kind === "available" ? n.staleReason : undefined;
          const [text, title, cls] =
            n.kind === "available"
              ? [
                  `有新版本 v${n.version} —— 去更新`,
                  stale
                    ? `最近一次客户端更新检查没成功（${stale}）；显示的版本号是上次成功查到的结果，` +
                      "可能已经不是最新。打开「设置 → 核心与数据更新」可以重试并更新。"
                    : "打开「设置 → 核心与数据更新」查看并更新（本卡只给入口，安装仍在那一页）",
                  stale ? "update-chip update-chip--new update-chip--stale" : "update-chip update-chip--new",
                ]
              : n.kind === "latest"
                ? [
                    // 点名「客户端」：本提示只谈客户端（task-193），说成光秃秃的「已是最新」
                    // 会被读成「核心/geo 也是最新的」，而那是另一条事实、另一处陈述。
                    `客户端已是最新（v${n.version}）`,
                    "上次客户端检查的结果：没有比当前更新的版本。点开可以手动再查一次。",
                    "update-chip",
                  ]
                : n.kind === "failed"
                  ? [
                      // 这一句**必须**看得出是「没查到」，不许读成「已是最新」；
                      // 并点名「客户端」——失败原因来自 `check_error_app`，与核心/geo 无关。
                      "客户端更新检查没成功 —— 所以不知道有没有新版本",
                      n.reason,
                      "update-chip update-chip--fail",
                    ]
                  : [`${n.reason}`, "点开可以去设置页手动检查一次", "update-chip"];
          return (
            <button
              type="button"
              className={cls}
              title={title}
              onClick={() => onNavigate("settings", "set-update")}
            >
              {text}
              {/* task-190：限定语必须**看得见**（不只放 title）；task-193：换用客户端专属
                  字段后，这里可以而且**应当**点名「客户端」（见 `updateNotice` 注释）。 */}
              {stale && (
                <span className="update-chip__note">
                  （最近一次客户端更新检查没成功；版本号是上次成功查到的）
                </span>
              )}
            </button>
          );
        })()}

        {/* 主操作（连接/断开）**只在顶栏渲染一个**（task-72）。
            这里原来还有一颗等价的「连接/断开」，同一个屏上出现两个 —— 而两者
            调的是同一个命令（顶栏 `toggleRun` → `run("stop"|"start", api.stop|api.start)`，
            与这里删掉的那颗逐字相同），所以它不是「另一条路径」，只是重复。
            保留顶栏那颗：它是全局控件，任何页面都在。
            恢复期间「不得可点」这条不变量搬到了 `runButtonDisabled`（有单测），
            没有随按钮一起消失。 */}
        <div className="dash__actions">
          <button className="btn btn--ghost" onClick={() => onNavigate("nodes")}>
            {connected ? "切换节点" : "选择节点"}
          </button>
          <button
            className="btn btn--ghost"
            disabled={probing || busy !== null || nodes.length === 0}
            onClick={() => void run("probe", () => api.testLatency())}
          >
            {probing ? <span className="spin" /> : null}
            测试延迟
          </button>
        </div>

        {/* 直连模式下连接按钮是禁用的 —— 说明原因，而不是让用户猜 */}
        {settings.mode === "direct" && (
          <div className="dash__hint">
            当前是<strong>直连</strong>模式，核心不会接管任何流量。要使用代理请先切换模式。
          </div>
        )}
      </section>

      {/* ---------------------------------------------------------- 流量 */}
      <section className="dash__traffic">
        <div className="dash__metric">
          <span className="dash__metric-arrow">↓</span>
          <span className="dash__metric-rate">{formatRate(traffic.rx_rate)}</span>
          <span className="dash__metric-total">累计 {formatBytes(traffic.rx_bytes)}</span>
        </div>
        <div className="dash__metric">
          <span className="dash__metric-arrow">↑</span>
          <span className="dash__metric-rate">{formatRate(traffic.tx_rate)}</span>
          <span className="dash__metric-total">累计 {formatBytes(traffic.tx_bytes)}</span>
        </div>
      </section>

      {/* ---------------------------------------------------------- 提示 */}
      {visible.map((n) => (
        <div key={n.key} className={`banner banner--${n.tone} dash__banner`}>
          <span>{n.icon}</span>
          <div style={{ flex: 1 }}>{n.text}</div>
          {n.action && <NoticeAction action={n.action} />}
        </div>
      ))}
      {rest.length > 0 && !showAllNotices && (
        <button className="dash__more" onClick={() => setShowAllNotices(true)}>
          还有 {rest.length} 条提示
        </button>
      )}

      {/* ------------------------------------------------- 诊断（默认折叠） */}
      <details className="dash__details">
        <summary>环境自检与诊断</summary>
        <p className="card__desc">
          TUN 模式需要两个外部条件：一个新版 Xray 核心，以及已授权的特权 helper。
          任何一项不满足，TUN 都会失败 —— 这里显示的就是它们当前的真实状态。
        </p>
        <dl className="kv">
          <dt>Xray 核心</dt>
          <dd>
            {core.path ? (
              <>
                <span className="mono">{core.path}</span>
                <br />
                {core.version ?? "（无法读取版本）"}{" "}
                {core.supports_native_tun ? (
                  <span className="badge badge--fast">支持原生 TUN</span>
                ) : (
                  <span className="badge badge--slow">需 &gt;= {core.min_native_tun_version}</span>
                )}
              </>
            ) : (
              <span style={{ color: "var(--danger)" }}>{core.error ?? "未找到"}</span>
            )}
          </dd>

          <dt>特权 helper</dt>
          <dd>
            {helper.reachable ? (
              <>
                <span className="badge badge--fast">已就绪</span> 版本 {helper.version ?? "?"} · 协议 v
                {helper.protocol ?? "?"}
                {helper.tun_active ? " · 有活跃隧道" : ""}
              </>
            ) : helper.socket_present ? (
              <span style={{ color: "var(--warn)" }}>
                已安装但无法连接{helper.error ? `：${helper.error}` : ""}
              </span>
            ) : (
              <span style={{ color: "var(--text-dim)" }}>
                未安装（仅影响 TUN 模式，系统代理模式不受影响）
              </span>
            )}
          </dd>

          <dt>核心进程</dt>
          <dd>{connected ? `运行中（pid ${runtime.pid ?? "?"}）` : "未运行"}</dd>

          <dt>运行中的配置</dt>
          <dd className="mono">{runtime.config_path ?? "（核心未运行）"}</dd>

          <dt>上次配置变更</dt>
          <dd>{formatTimestamp(runtime.started_at_unix)}</dd>
        </dl>

        <div className="row row--wrap" style={{ marginTop: 12 }}>
          <button
            className="btn btn--ghost"
            disabled={busy !== null}
            onClick={() => void run("refresh-subs", () => api.refreshSubscriptions())}
          >
            更新全部订阅
          </button>
          <button className="btn btn--ghost" onClick={() => void api.openDataDir()}>
            打开数据目录
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("logs")}>
            查看日志
          </button>
          <button className="btn btn--ghost" onClick={() => onNavigate("settings")}>
            全部设置
          </button>
        </div>
      </details>
    </div>
  );
}

/**
 * 把「需要用户知道的事」按严重度收集起来。
 *
 * 旧版把这些写成 5 个平铺的 `{cond && <Banner/>}`，顺序是代码顺序而不是
 * 紧急程度，于是「流量还没走代理」可能排在「helper 没装」下面。
 * 收集后由调用方只显示最急的一条 —— 需要看全时也仍然拿得到。
 *
 * 导出是为了让「哪条 notice 带二次确认、问句里有没有真实后果」能被单测直接断言
 * （渲染整个仪表盘需要一整套 store，那条断言就会没人愿意跑）。
 */
export function collectNotices(
  snapshot: AppSnapshot,
  run: ReturnType<typeof useStore>["run"],
  onNavigate: (view: string, target?: string) => void,
): Notice[] {
  const { core, helper, notice, runtime } = snapshot;
  const out: Notice[] = [];

  // rank 越小越急：先「现在就是坏的」，再「需要修」，最后「仅供参考」。
  if (runtime.last_error) {
    out.push({
      key: "last-error",
      tone: "error",
      icon: "✕",
      rank: 0,
      text: <>上次运行出错：{runtime.last_error}</>,
    });
  }

  if (!core.path) {
    out.push({
      key: "no-core",
      tone: "error",
      icon: "✕",
      rank: 1,
      text: (
        <>
          没有找到 Xray 核心。请把 <span className="mono">xray</span> 放到 app bundle 的 Resources
          目录，或在设置里指定它的绝对路径。
        </>
      ),
    });
  }

  if (helper.stale_session) {
    out.push({
      key: "stale",
      tone: "warn",
      icon: "⚠︎",
      rank: 2,
      text: (
        <>
          检测到上次异常退出遗留的网络配置（会话 {helper.stale_session}）。这可能导致网络异常，
          建议立即回滚。
        </>
      ),
      action: {
        label: "立即修复",
        /**
         * 二次确认（task-68）。后果全部来自代码，不是推测：
         * * `Request::Restore` 在 helper 侧是**无条件**的强清理
         *   （`crates/xt-helper/src/server.rs:301-332`：`tear_down_live_session()`
         *   先拆内存里的活会话、关掉 utun fd，再 `force_cleanup()` 按磁盘快照还原
         *   路由与 DNS）；
         * * `restore_stale`（`apps/desktop/src/commands/helper.rs:56-74`）只发这一个
         *   请求并重建快照，**不调用 `start_core`** —— 所以**不会自动重连**。
         * 这两条是唯一可确认的后果；其余（耗时、是否需要重装 helper）不写。
         */
        confirm: {
          question:
            "回滚网络配置会还原 helper 装的路由与 DNS，并拆掉当前正在生效的那条隧道（utun 网卡也会移除）——网络会回到直连；如果你正连着，连接会断。",
          confirmLabel: "确认回滚",
        },
        run: () => void run("restore", () => api.restoreStale()),
      },
    });
  }

  if (core.path && !core.supports_native_tun) {
    out.push({
      key: "old-core",
      tone: "warn",
      icon: "⚠︎",
      rank: 3,
      text: (
        <>
          当前核心的 TUN 实现不完整（需要 &gt;= {core.min_native_tun_version}）。
          低于该版本时核心只会创建网卡，不会配置地址与路由，TUN 模式不可用。
        </>
      ),
    });
  }

  if (notice) {
    out.push({
      key: "notice",
      tone: "info",
      icon: "ℹ︎",
      rank: 4,
      text: <>{notice}</>,
      action: {
        label: "去处理",
        // helper 未装是最常见的来源，直接带去设置页比让用户自己找路更省事。
        //
        // **但必须带上目标分节**：设置页现在是两级结构（选中哪一类只显示那一类），
        // 只跳到「设置」会落在默认分类上，而「特权助手」在「系统与助手」里 ——
        // 那等于把用户带到一个看不到待处理项的地方，比不给入口更糟。
        run: () =>
          onNavigate(
            helper.socket_present ? "logs" : "settings",
            helper.socket_present ? undefined : "set-helper",
          ),
      },
    });
  }

  return out.sort((a, b) => a.rank - b.rank);
}

/** 「已运行 12 分钟」这种相对时间。只用于状态区，精度到分钟就够。 */
function elapsed(startedAtUnix: number): string {
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - startedAtUnix);
  if (secs < 60) return `${secs} 秒`;
  if (secs < 3600) return `${Math.floor(secs / 60)} 分钟`;
  const hours = Math.floor(secs / 3600);
  if (hours < 24) return `${hours} 小时 ${Math.floor((secs % 3600) / 60)} 分钟`;
  return `${Math.floor(hours / 24)} 天`;
}

/*
 * 这里原来有 `connectControl()`：仪表盘那颗「连接/断开」按钮的呈现（含
 * 「恢复期间必须禁用」）。
 *
 * task-72 把那颗按钮收敛掉了 —— 它与顶栏那颗是**同一个命令**的等价按钮
 * （两处都是 `run("stop"|"start", api.stop|api.start)`），同屏出现两个「断开」
 * 只是重复。该不变量没有随之消失，而是搬到了 `topbarStatus.runButtonDisabled()`
 * （顶栏唯一的那个按钮），并保持有单测。
 */
