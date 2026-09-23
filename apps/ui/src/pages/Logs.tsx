/**
 * 日志页。
 *
 * # 这一版改了什么
 *
 * 1. **等级筛选带上计数。** 以前要判断「有没有报错」必须先点一下「错误」——
 *    而现在这个数字直接写在标签上，一眼就能看出该不该关注。这是这一页
 *    最实际的一处改进。
 * 2. **行首色条 + 定宽时间戳。** 原来只靠文字颜色区分等级，在一屏滚动的
 *    等宽文本里很难扫；现在左侧有一条 2px 的色条，且时间戳定宽，
 *    多行消息的续行不会再跳位。
 * 3. **工具栏分组。** 原来筛选、搜索、自动滚动、复制、诊断、清空六个控件
 *    等权排成一行。现在左边是「看什么」（等级 + 搜索），右边是「做什么」
 *    （跟随、复制、诊断、清空），中间用弹性空隙分开。
 * 4. 来源（app/core/helper）以弱化标签显示 —— 排查时经常要先分清
 *    「是 App 说的还是核心说的」。
 *
 * 保留的行为：跟随滚动只在开启时生效（否则用户往上翻历史会被不断打断）、
 * 多行消息按原样保留换行与缩进、复制走剪贴板且失败时退化为控制台输出。
 */

import { useMemo, useState } from "react";
import { api } from "../ipc";
import { InlineConfirm } from "../InlineConfirm";
import { useFollowScroll, usePreserveReadingPosition } from "../useFollowScroll";
import { useStore } from "../store";
import IncidentReport, { CopyButton } from "../IncidentReport";
import { formatTimestamp } from "../types";

const LEVELS = ["all", "info", "warn", "error", "debug"] as const;
type Level = (typeof LEVELS)[number];

const LEVEL_LABEL: Record<Level, string> = {
  all: "全部",
  info: "信息",
  warn: "警告",
  error: "错误",
  debug: "调试",
};

/** 只有真正出问题时才值得用颜色强调的等级。 */
const NOTABLE: readonly Level[] = ["error", "warn"];

export default function Logs() {
  const { logs, logsLoad, reloadLogs, clearLogs, runVoid, snapshot } = useStore();
  // 「核心有没有在跑」取自**后端快照**（`runtime.running`），不是按日志条数或时间猜。
  // 它只用来区分**两种不同的空**：核心启动过但没输出 / 核心从没启动过。
  const coreRunning = snapshot?.runtime.running === true;
  /**
   * task-120：「启动过」的判据是 `runtime.started_at_unix`，**不是** `!running`。
   * 它只在核心启动时写一次（`supervisor.rs:700`），停止时不清
   * （`commands/core.rs::runtime_after_stop` 只改 running/pid）⇒ 非 null 就说明
   * 这次 App 会话里核心确实起来过。停过之后再说「还没启动过」是给错因。
   */
  const coreStarted = (snapshot?.runtime.started_at_unix ?? null) !== null;
  /** 启动时刻本身（拿不到就**不写**时刻，说明「读不到」—— 不编一个时间出来）。 */
  const startedUnix = snapshot?.runtime.started_at_unix ?? null;
  const [diagnostics, setDiagnostics] = useState<string | null>(null);
  const [level, setLevel] = useState<Level>("all");
  const [query, setQuery] = useState("");

  // 各等级的条数：用于筛选标签上的计数。基于**全量**日志算，
  // 而不是基于当前筛选结果 —— 否则切换等级时数字会互相矛盾。
  const counts = useMemo(() => {
    const c: Record<string, number> = { all: logs.length, info: 0, warn: 0, error: 0, debug: 0 };
    for (const l of logs) {
      c[l.level] = (c[l.level] ?? 0) + 1;
    }
    return c;
  }, [logs]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return logs.filter((l) => {
      if (level !== "all" && l.level !== level) return false;
      if (q && !l.message.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [logs, level, query]);

  // 跟随滚动：行为与判据见 `useFollowScroll`。
  //
  // 关键修正是**用滚动位置表达意图**：用户往上翻历史时自动暂停跟随，
  // 滚回底部自动恢复。原来的实现是「只要开关开着就每次滚到底」，
  // 于是用户往上翻时会被下一条日志立刻拽回底部（实测 scrollTop 50 → 2065），
  // 表现就是「根本滚不动」。
  //
  // 这里传的是**内容版本**（长度 + 最新一行的身份），**不是长度**：
  // 缓冲满员后长度恒为 MAX_UI_LOGS，只传长度会让跟随在这之后静默失效
  // （新行不再滚进视野）—— 见 `logsDomStability.test.tsx` 的反例测试。
  const lastVisibleSeq = filtered.length ? filtered[filtered.length - 1]!.seq : 0;
  const { boxRef, bottomRef, follow, setFollow, onScroll } = useFollowScroll(
    `${filtered.length}:${lastVisibleSeq}`,
  );

  // 缓冲满员后从**前面**裁行：把阅读位置钉住（原理见 hook 文档）。
  // `enabled` 只在跟随关闭时成立 —— 跟随开着时我们本来就要贴底。
  usePreserveReadingPosition(boxRef, !follow, logs[0]?.seq ?? null);

  const exportLogs = async () => {
    const text = filtered
      .map((l) => `[${new Date(l.ts_unix * 1000).toISOString()}] ${l.source}/${l.level} ${l.message}`)
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // 剪贴板可能被拒绝；退化成在控制台输出，至少不会静默失败。
      console.log(text);
    }
  };

  return (
    <div className="logs-page">
      <div className="logs-bar">
        <div className="segmented">
          {LEVELS.map((l) => (
            <button
              key={l}
              className={level === l ? "is-active" : ""}
              onClick={() => setLevel(l)}
              // task-120：这个数字是**已加载窗口**里的条数（初始 `tailLogs(500)`、
              // 之后封顶 `MAX_UI_LOGS = 1500`，见 `store.tsx`），不是文件里的总数。
              // 原来说「错误：1 条」—— 用户会读成「整个日志只有 1 条错误」。
              // 判据就是手里这份 `logs`，所以直接把它的口径写出来。
              title={`${LEVEL_LABEL[l]}：已加载的 ${logs.length} 条中有 ${counts[l] ?? 0} 条`}
            >
              {LEVEL_LABEL[l]}
              {(counts[l] ?? 0) > 0 && (
                <span className={`logs-bar__count${NOTABLE.includes(l) ? ` logs-bar__count--${l}` : ""}`}>
                  {counts[l]}
                </span>
              )}
            </button>
          ))}
        </div>

        <input
          type="text"
          className="logs-bar__search"
          placeholder="过滤关键字"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />

        <span className="spacer" />

        <label className="logs-bar__follow">
          <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
          跟随
        </label>
        <button className="btn btn--ghost" onClick={() => void exportLogs()}>
          复制
        </button>
        <button
          className="btn btn--ghost"
          onClick={() => void runVoid("diag", async () => setDiagnostics(await api.diagnostics()))}
        >
          诊断
        </button>
        <InlineConfirm
          label="清空"
          className="btn btn--ghost btn--danger"
          title="删除日志文件（无法撤销）"
          question="清空日志？会删除日志文件本身，无法撤销。"
          confirmLabel="确认清空"
          onConfirm={() => void clearLogs()}
        />
      </div>

      {/* 读取失败必须留痕：给出后端原文 + 一个真的能再取一次的动作。
          以前这里是静默 catch，于是「读不到」与「没有日志」在界面上无法区分，
          而下面的空态文案又把它解释成「核心还没启动过」—— 那是**错误的原因**。 */}
      {logsLoad.phase === "failed" && (
        <div className="banner banner--error logs__banner" role="alert">
          <strong>读取日志失败</strong>
          <span className="logs__banner-text">{logsLoad.error ?? "（后端没有给出原因）"}</span>
          <span className="spacer" />
          <button className="btn btn--ghost" onClick={() => void reloadLogs()}>
            重试
          </button>
        </div>
      )}

      {diagnostics && (
        <div className="logs-diag">
          <div className="logs-diag__head">
            <strong>诊断报告</strong>
            <span className="logs-diag__note">
              {/* task-124：task-120 收尾时这里写的是「节点地址、域名与 IP:port
                  **会原样保留** —— 贴到公开 issue 前请自己核对一遍」。那句话**如实**，
                  而如实描述的恰恰是一个缺陷：报告会把用户自己的服务器地址留在里面。
                  现在后端把脱敏做够了（`commands/diagnostics.rs::redact_secrets`）：
                  · 判据来自**当前节点列表**（`Node::address` / SNI / 传输层 host /
                    节点名里的域名段）—— 命中即换成 `<addr>`；
                  · **没命中列表的公网 IP** 也换：域名节点在日志里出现的是**解析后的 IP**，
                    已经被切走的旧节点 IP 更不在列表里；
                  · **本机管道地址保留**（`127/8`、RFC1918、`::1`、ULA）—— 它们不带身份，
                    却是排查「回环洞」这类问题的命门；
                  · 公开目标域名（`www.baidu.com`）保留，否则报告没法看。
                  只有一种形态覆盖不到，所以直接点名写出来：base64 载荷。 */}
              已抹掉：订阅 URL 的凭据、UUID 形状的 token、<strong>节点地址/域名</strong>与
              IP:port（替换成 <code>&lt;addr&gt;</code>）。App/核心/助手版本、时间，以及
              <code>www.baidu.com</code> 这类公开目标域名与本机地址（<code>127.0.0.1</code>）
              保留 —— 排查要用。唯一覆盖不到的形态是 <strong>base64 载荷</strong>
              （如 vmess 分享链接里那段），日志里出现时请手动删掉再贴。
            </span>
            <span className="spacer" />
            {/* task-124：这里原来是**裸** `navigator.clipboard.writeText(diagnostics)`
                （未 catch）—— 剪贴板被拒（权限/非聚焦窗口）时界面毫无提示，
                用户以为复制成功、贴出去却是空的。改用与设置页/「报告问题」同一个
                `CopyButton`：失败给 `role="alert"` + 可手动选中的 textarea 兜底。 */}
            <CopyButton label="复制" text={diagnostics} className="btn btn--ghost" />
            <button className="btn btn--ghost" onClick={() => setDiagnostics(null)}>
              关闭
            </button>
          </div>
          <pre className="logs-diag__body">{diagnostics}</pre>
        </div>
      )}

      <div className="logs" ref={boxRef} onScroll={onScroll}>
        {filtered.length === 0 ? (
          <div className="logs__empty">
            {logs.length > 0 ? (
              <>当前筛选条件下没有日志（共 {logs.length} 条，换个等级或清空关键字试试）。</>
            ) : logsLoad.phase === "loading" ? (
              <>正在读取日志…</>
            ) : logsLoad.phase === "failed" ? (
              <>
                日志没读到 —— 这不等于「没有日志」，读日志本身就失败了（原因见上方），
                点「重试」再取一次。
              </>
            ) : coreRunning ? (
              /*
                task-128（B9）：原来写「**刚启动时**这样是正常的」——「刚启动」是从
                `running === true` **猜**出来的，而核心可能已经跑了几小时（用户点了
                「清空」把内存与文件都删了，`store.rs:213-224`），那时这句话把用户
                引向错误的原因。这里改成用 `runtime.started_at_unix` 把**启动时刻**
                如实摆出来，让用户自己判断，同时把两种真实可能都写上。
              */
              <>
                核心已在运行
                {startedUnix !== null
                  ? `（本次启动于 ${formatTimestamp(startedUnix)}）`
                  : "（读不到本次的启动时刻）"}
                ，但当前还没有日志 —— 可能是刚启动还没输出，也可能是日志刚被清空；
                有输出会被实时转发到这里。
              </>
            ) : coreStarted ? (
              // task-120：原来这里只有「核心还没启动过」一句，判据却只是
              // `runtime.running === false`（现在时）。而「启动过、现在停了」
              // 与「从来没启动过」是两件事：`runtime.started_at_unix` 只在
              // 核心启动时写一次（`supervisor.rs:700`），停止时**不清**
              // （`commands/core.rs::runtime_after_stop` 只改 running/pid）。
              // 另外：日志文件全部读不到时后端回退内存并返回 `Ok(空)`、不上报失败
              // （`diagnostics.rs:52-58`），所以「读不到」也得算进这一句里。
              <>还没有日志：核心启动过（{formatTimestamp(snapshot?.runtime.started_at_unix ?? null)}），
                但本次没有输出 —— 也可能是日志刚被清空，或文件读不到。</>
            ) : (
              <>还没有日志：核心还没启动过。启动后它的 stdout/stderr 会被实时转发到这里。</>
            )}
          </div>
        ) : (
          filtered.map((line) => (
            <div
              // **必须是稳定身份**：`seq` 在入库时分配一次、永不改变。
              // 这里以前是 `${line.ts_unix}-${i}` —— 下标一旦进入 key，缓冲满员后
              // 每来一行新日志都会让所有 key 前移一位，React 就卸载重建整列表
              // （1500 个节点/行），用户看到的就是「关闭跟随后日志还一直跳动」。
              key={line.seq}
              // 供「裁剪时钉住阅读位置」量幸存行的位移（见 usePreserveReadingPosition）。
              data-log-seq={line.seq}
              className={`log-line log-line--${line.level}`}
            >
              <span className="log-line__ts">{formatClock(line.ts_unix)}</span>
              <span className="log-line__src">{line.source}</span>
              <span className="log-line__msg">{line.message}</span>
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>

      {/* 「报告问题」（task-131）。挂在这里的理由写在 `IncidentReport.tsx` 顶部：
          要报的问题，证据就是这一页的日志；而「报告问题」的第一步产出的正是
          「日志 + 运行状态」的本地包。**刻意挂在诊断块之外** —— 那块（含脱敏说明）
          归 task-124，本卡一行都不碰。 */}
      <IncidentReport />
    </div>
  );
}

function formatClock(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
