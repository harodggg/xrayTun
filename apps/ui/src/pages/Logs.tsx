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
  // 它只用来区分**两种不同的空**：核心没启动过 / 启动过但还没输出。
  const coreRunning = snapshot?.runtime.running === true;
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
              // 有内容才显示计数，0 会让标签变吵
              title={`${LEVEL_LABEL[l]}：${counts[l] ?? 0} 条`}
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
              已抹掉订阅 URL、节点地址与 UUID —— 可以直接贴到公开的 issue 里
            </span>
            <span className="spacer" />
            <button className="btn btn--ghost" onClick={() => void navigator.clipboard.writeText(diagnostics)}>
              复制
            </button>
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
              <>核心已在运行，但还没有产生日志 —— 刚启动时这样是正常的，有输出会被实时转发到这里。</>
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
    </div>
  );
}

function formatClock(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
