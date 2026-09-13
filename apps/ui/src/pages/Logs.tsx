import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../ipc";
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

export default function Logs() {
  const { logs, clearLogs, runVoid } = useStore();
  const [diagnostics, setDiagnostics] = useState<string | null>(null);
  const [level, setLevel] = useState<Level>("all");
  const [query, setQuery] = useState("");
  const [follow, setFollow] = useState(true);
  const bottomRef = useRef<HTMLDivElement>(null);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return logs.filter((l) => {
      if (level !== "all" && l.level !== level) return false;
      if (q && !l.message.toLowerCase().includes(q)) return false;
      return true;
    });
  }, [logs, level, query]);

  // 自动滚到底：只有在「跟随」打开时才做，否则用户往上翻历史会被不断打断。
  useEffect(() => {
    if (follow) {
      bottomRef.current?.scrollIntoView({ block: "end" });
    }
  }, [filtered.length, follow]);

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
    <>
      <div className="card">
        <div className="row row--wrap">
          <div className="segmented">
            {LEVELS.map((l) => (
              <button key={l} className={level === l ? "is-active" : ""} onClick={() => setLevel(l)}>
                {LEVEL_LABEL[l]}
              </button>
            ))}
          </div>
          <input
            type="text"
            placeholder="过滤关键字"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            style={{ flex: 1, minWidth: 160 }}
          />
          <label className="row" style={{ gap: 6, fontSize: 12, color: "var(--text-dim)" }}>
            <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} />
            自动滚动
          </label>
          <button className="btn" onClick={() => void exportLogs()}>
            复制
          </button>
          <button
            className="btn"
            onClick={() => void runVoid("diag", async () => setDiagnostics(await api.diagnostics()))}
          >
            诊断
          </button>
          <button className="btn btn--ghost" onClick={clearLogs}>
            清空
          </button>
        </div>
      </div>

      {diagnostics && (
        <div className="card">
          <div className="row" style={{ marginBottom: 8 }}>
            <h2 className="card__title" style={{ margin: 0 }}>
              诊断报告
            </h2>
            <div className="spacer" />
            <button className="btn" onClick={() => void navigator.clipboard.writeText(diagnostics)}>
              复制
            </button>
            <button className="btn btn--ghost" onClick={() => setDiagnostics(null)}>
              关闭
            </button>
          </div>
          <p className="card__desc">
            报告里已抹掉订阅 URL、节点地址与 UUID —— 可以直接贴到公开的 issue 里。
          </p>
          <pre
            className="mono"
            style={{
              whiteSpace: "pre-wrap",
              maxHeight: 260,
              overflow: "auto",
              fontSize: 11,
              background: "#0a0e16",
              padding: 10,
              borderRadius: 6,
              margin: 0,
            }}
          >
            {diagnostics}
          </pre>
        </div>
      )}

      <div className="logs">
        {filtered.length === 0 ? (
          <div style={{ color: "var(--text-faint)" }}>
            没有匹配的日志。核心的 stdout/stderr 会被实时转发到这里；
            如果一条都没有，通常意味着核心还没启动过。
          </div>
        ) : (
          filtered.map((line, i) => (
            <div key={`${line.ts_unix}-${i}`} className={`log-line log-line--${line.level}`}>
              <span className="log-line__ts">{formatClock(line.ts_unix)}</span>
              <span className="log-line__msg">{line.message}</span>
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </>
  );
}

function formatClock(unixSeconds: number): string {
  const d = new Date(unixSeconds * 1000);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}
