import { useMemo, useState } from "react";
import { api, errorText } from "../ipc";
import { useStore } from "../store";
import { latencyTier, nodeSummary, type Node } from "../types";

export default function Nodes() {
  const { snapshot, busy, run, probing } = useStore();
  const [query, setQuery] = useState("");
  const [adding, setAdding] = useState(false);
  const [link, setLink] = useState("");
  const [addError, setAddError] = useState<string | null>(null);

  const nodes = snapshot?.nodes ?? [];
  const latency = snapshot?.latency ?? {};
  const selectedId = snapshot?.settings.selected_node ?? null;

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter(
      (n) =>
        n.name.toLowerCase().includes(q) ||
        n.address.toLowerCase().includes(q) ||
        nodeSummary(n).toLowerCase().includes(q),
    );
  }, [nodes, query]);

  const submitManual = async () => {
    setAddError(null);
    if (!link.trim()) return;
    const ok = await run("add-node", () => api.addManualNode(link.trim()));
    if (ok) {
      setLink("");
      setAdding(false);
    } else {
      setAddError("解析失败，请检查链接格式");
    }
  };

  return (
    <>
      <div className="card">
        <div className="row row--wrap">
          <input
            type="text"
            placeholder="搜索节点名称、地址或协议"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            style={{ flex: 1, minWidth: 200 }}
          />
          <button
            className="btn"
            disabled={probing || busy !== null || nodes.length === 0}
            onClick={() => void run("probe", () => api.testLatency(filtered.map((n) => n.id)))}
          >
            {probing ? <span className="spin" /> : null}
            测试{query ? "筛选结果" : "全部"}延迟
          </button>
          <button className="btn" onClick={() => setAdding((v) => !v)}>
            {adding ? "取消" : "手动添加"}
          </button>
        </div>

        {adding && (
          <div style={{ marginTop: 12 }}>
            <div className="field">
              <label>粘贴节点</label>
              <textarea
                rows={5}
                placeholder={"三种都行：\n  1) 分享链接    vless://… / vmess://… / trojan://… / ss://…\n  2) 订阅正文    多行链接，或 base64 / Clash YAML\n  3) Clash 单条   {name: x, type: vless, server: …, port: …, uuid: …}"}
                value={link}
                onChange={(e) => setLink(e.target.value)}
                style={{ resize: "vertical", fontFamily: "var(--mono)", fontSize: 11 }}
              />
              <div className="field__hint">
                协议支持 vmess / vless / trojan / shadowsocks / socks / http。
                ShadowsocksR（ssr://）不被 Xray 支持，会明确报错而不是静默忽略。
                <br />
                从机场文档复制的 Clash 片段可以直接贴，不需要自己包一层{" "}
                <span className="mono">proxies:</span> 外壳。
              </div>
            </div>
            {addError && (
              <div className="banner banner--error" style={{ marginBottom: 10 }}>
                <span>✕</span>
                <div>{addError}</div>
              </div>
            )}
            <button className="btn btn--primary" disabled={!link.trim() || busy !== null} onClick={() => void submitManual()}>
              解析并添加
            </button>
          </div>
        )}
      </div>

      {filtered.length === 0 ? (
        <div className="empty">
          {nodes.length === 0 ? (
            <>
              还没有任何节点。
              <br />
              去「订阅」页添加一个机场订阅，或在这里手动粘贴分享链接。
            </>
          ) : (
            <>没有匹配「{query}」的节点。</>
          )}
        </div>
      ) : (
        <div className="list">
          {filtered.map((node) => (
            <NodeRow
              key={node.id}
              node={node}
              selected={node.id === selectedId}
              latencyMs={latency[node.id]?.server_rtt_ms ?? null}
              available={latency[node.id]?.available ?? null}
              latencyError={latency[node.id]?.error ?? null}
              busy={busy !== null}
              onSelect={() => void run("select", () => api.selectNode(node.id))}
              onDelete={() => void run("delete", () => api.deleteNode(node.id))}
            />
          ))}
        </div>
      )}
    </>
  );
}

function NodeRow({
  node,
  selected,
  latencyMs,
  available,
  latencyError,
  busy,
  onSelect,
  onDelete,
}: {
  node: Node;
  selected: boolean;
  /** 本地 → 服务器的 TCP 握手 RTT（中位数）。这是「延迟」。 */
  latencyMs: number | null;
  /** 经该节点能不能取到东西。`null` 表示还没测过。 */
  available: boolean | null;
  latencyError: string | null;
  busy: boolean;
  onSelect: () => void;
  onDelete: () => void;
}) {
  const tier = latencyTier(latencyMs);
  const fromSubscription = node.source.kind === "subscription";

  return (
    <div className={`list__row${selected ? " is-selected" : ""}`} onClick={busy ? undefined : onSelect}>
      <span className={`dot${selected ? " dot--on" : ""}`} />
      <div className="list__main">
        <div className="list__name">{node.name}</div>
        <div className="list__meta">
          {nodeSummary(node)} · {node.address}:{node.port}
          {fromSubscription ? "" : " · 手动添加"}
          {node.tls.server_name ? ` · SNI ${node.tls.server_name}` : ""}
        </div>
      </div>

      {/* 延迟与可用性分开显示，因为它们回答的是两个问题：
          「离我多远」和「能不能用」。合成一个数字会让排序骗人 ——
          经节点请求一个固定靶点量到的是「本地→服务器→靶点→回来」，
          其中「服务器→靶点」那段取决于服务器离靶点有多远，
          可能让一个很远的节点看起来比近的更快。 */}
      <span className={`badge badge--${tier}`} title={latencyError ?? ""}>
        {latencyMs !== null ? `${latencyMs} ms` : latencyError ? "—" : "未测"}
      </span>
      <span
        className={`badge badge--${available === null ? "unknown" : available ? "fast" : "slow"}`}
        title={
          available === null
            ? "尚未测试"
            : available
              ? "经该节点可以正常取到数据"
              : (latencyError ?? "经该节点取不到数据")
        }
      >
        {available === null ? "可用性未测" : available ? "可用" : "不可用"}
      </span>

      <button
        className="btn btn--ghost btn--danger"
        disabled={busy}
        title="删除该节点"
        onClick={(e) => {
          e.stopPropagation();
          onDelete();
        }}
      >
        删除
      </button>
    </div>
  );
}

/** 供外部复用：把一次探测失败的原因显示成人话。 */
export function describeProbeError(e: unknown): string {
  return errorText(e);
}
