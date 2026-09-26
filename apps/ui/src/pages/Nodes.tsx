/**
 * 节点页。
 *
 * # 这一版改了什么
 *
 * 1. **把「距离」与「能不能用」写进徽章本身。** 原来两者只是一个 ms 数字和
 *    一个「可用/不可用」，真正的区别藏在 `title` 提示里 —— 而提示只有鼠标
 *    悬停才看得到。代码注释自己就记着这个问题：「绿色 55ms 紧挨红色不可用，
 *    读起来就是自相矛盾」。现在徽章上带标签（`距离 55 ms` / `不可用`），
 *    不用悬停也读得对。
 * 2. **搜索与动作分成两组。** 原来搜索框、测延迟、手动添加挤在一行里，
 *    而「测延迟」的作用对象（全部还是筛选结果）只在按钮文案里体现。
 *    现在筛选条件与结果条数一起显示，用户知道自己在测什么。
 * 3. **当前节点用左侧强调条 + 明确标签。** 原来只有一个绿点，和其它行
 *    区分度很低，而「哪台正在用」是这一页最要紧的信息。
 * 4. 行内动作收成图标式文本按钮，减少每行的视觉重量（列表页保留高密度，
 *    但把装饰性重量降下来）。
 */

import { useMemo, useState, type KeyboardEvent } from "react";
import { api, errorText } from "../ipc";
import { CopyButton } from "../IncidentReport";
import { InlineConfirm } from "../InlineConfirm";
import SnapshotFallback from "../SnapshotState";
import { useStore } from "../store";
import {
  formatTimestamp,
  latencyTier,
  nodeSummary,
  type Node,
  type NodeExport,
  type ProbeResult,
} from "../types";

export default function Nodes() {
  const { snapshot, busy, run, probing } = useStore();
  const [query, setQuery] = useState("");
  const [adding, setAdding] = useState(false);
  const [link, setLink] = useState("");
  const [addError, setAddError] = useState<string | null>(null);
  const [exported, setExported] = useState<NodeExport | null>(null);
  const [exportError, setExportError] = useState<string | null>(null);

  const nodes = snapshot?.nodes ?? [];
  const latency = snapshot?.latency ?? {};
  const selectedId = snapshot?.settings.selected_node ?? null;

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter((n) =>
      [n.name, n.address, n.protocol.kind].some((v) => v.toLowerCase().includes(q)),
    );
  }, [nodes, query]);

  /**
   * task-23 A1：读不到快照时**不能**继续往下渲染「还没有任何节点」——
   * 那是把「没读到」说成「没有」，属于给错原因（详见 `SnapshotState.tsx`）。
   * 放在 `useMemo` **之后**：hook 的调用顺序必须与其它 render 一致。
   */
  if (!snapshot) return <SnapshotFallback />;

  /**
   * task-23 B1：行内 roving tabindex 的落点。
   *
   * 选中项优先；**一个都没选中时第一条**也要能 Tab 进来 —— 否则键盘用户
   * 永远进不了这个列表（`tabIndex={selected ? 0 : -1}` 在 selected 为 null
   * 时会让所有行都不可聚焦）。
   */
  const rovingId = selectedId ?? filtered[0]?.id ?? null;

  /**
   * 行的键盘操作（task-23 B1）。原来是 `<div onClick>`：键盘用户 Tab 只能落到
   * 行内的「二维码 / 删除」，**没办法切换节点**，读屏也读不出这一组是单选、选没选中。
   *
   * 方向键按**视觉顺序**在组内移动并同时选中（与 macOS 单选列表一致）；
   * Enter/Space 只选当前行。
   */
  const onRowKeyDown = (e: KeyboardEvent<HTMLDivElement>, node: Node, index: number) => {
    if (busy !== null) return;
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void run("select", () => api.selectNode(node.id));
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const nextIndex = e.key === "ArrowDown" ? index + 1 : index - 1;
    const next = filtered[nextIndex];
    if (!next) return;
    const rows = e.currentTarget.parentElement?.querySelectorAll<HTMLElement>('[role="radio"]');
    rows?.[nextIndex]?.focus();
    void run("select", () => api.selectNode(next.id));
  };

  const openExport = async (nodeId: string) => {
    setExportError(null);
    try {
      setExported(await api.exportNode(nodeId));
    } catch (e) {
      setExportError(errorText(e));
    }
  };

  const submitManual = async () => {
    const ok = await run("add-node", () => api.addManualNode(link.trim()));
    if (ok) {
      setLink("");
      setAdding(false);
      setAddError(null);
      return;
    }
    /*
      task-151：这里原来写「**解析失败，请检查链接格式**」—— 那是**替后端编原因**。
      `run()` 返回 `false` 有三种来源且**调用方无法区分**（见 `store.tsx` 里 `run` 的文档）：
      ① busy 竞态（本次点击根本没执行）；② 命令抛错（原因在 `error` 里）；
      ③ 命令成功但快照形状异常（守卫拦下，原因也在 `error` 里）。
      而真实的拒绝原因**已经由后端给出并显示在页面顶部的横幅里**（例如
      「ShadowsocksR 不被 Xray 支持，请改用 ss/vless/trojan」）。
      ⇒ 拿不准就**只陈述确定的事**（这次添加没有生效）+ 指向权威来源，不编一个具体原因。
      注：本页的「解析并添加」按钮在空输入时是禁用的，所以「输入不合法」这个**本地可确定**的
      原因在这里并不存在 —— 没有任何本地校验可以断言。
    */
    setAddError("这次添加没有生效 —— 原因见页面上方的提示条（后端给出的原文在那里）。");
  };

  return (
    <div className="page">
      <div className="nodes-bar">
        <input
          type="text"
          placeholder="搜索名称、地址或协议"
          aria-label="搜索节点"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <span className="nodes-bar__count">
          {query ? `${filtered.length} / ${nodes.length}` : `${nodes.length} 个节点`}
        </span>
        <span className="spacer" />
        <button
          className="btn"
          disabled={probing || busy !== null || nodes.length === 0}
          onClick={() => void run("probe", () => api.testLatency(filtered.map((n) => n.id)))}
        >
          {probing ? <span className="spin" /> : null}
          测试{query ? "筛选结果" : "全部"}延迟
        </button>
        <button className="btn btn--ghost" onClick={() => setAdding((v) => !v)}>
          {adding ? "取消" : "手动添加"}
        </button>
      </div>

      {adding && (
        <section className="page__sec">
          <div className="field">
            <label>粘贴节点</label>
            <textarea
              rows={5}
              placeholder={
                "三种都行：\n  1) 分享链接    vless://… / vmess://… / trojan://… / ss://…\n  2) 订阅正文    多行链接，或 base64 / Clash YAML\n  3) Clash 单条   {name: x, type: vless, server: …, port: …, uuid: …}"
              }
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
            <div className="banner banner--error" role="alert">
              <span>✕</span>
              <div>{addError}</div>
            </div>
          )}
          <button
            className="btn btn--primary"
            disabled={!link.trim() || busy !== null}
            onClick={() => void submitManual()}
          >
            解析并添加
          </button>
        </section>
      )}

      {filtered.length === 0 ? (
        <div className="note">
          {nodes.length === 0 ? (
            <>
              还没有任何节点。去「订阅」页添加一个机场订阅，或在这里手动粘贴分享链接。
            </>
          ) : (
            <>没有匹配「{query}」的节点（共 {nodes.length} 个）。</>
          )}
        </div>
      ) : (
        // task-23 B1：整组是**单选**（选中哪一个节点），所以用 radiogroup/radio，
        // 而不是给行加 role="button"（那会和行内两个真按钮形成「按钮套按钮」）。
        <div className="list" role="radiogroup" aria-label="节点">
          {filtered.map((node, index) => (
            <NodeRow
              key={node.id}
              node={node}
              selected={node.id === selectedId}
              probe={latency[node.id]}
              busy={busy !== null}
              tabIndex={busy !== null || node.id !== rovingId ? -1 : 0}
              onKeyDown={(e) => onRowKeyDown(e, node, index)}
              onSelect={() => void run("select", () => api.selectNode(node.id))}
              onDelete={() => void run("delete", () => api.deleteNode(node.id))}
              onExport={() => void openExport(node.id)}
            />
          ))}
        </div>
      )}

      {(exported || exportError) && (
        <div
          className="modal"
          onClick={() => {
            setExported(null);
            setExportError(null);
          }}
        >
          <div className="modal__box" onClick={(e) => e.stopPropagation()}>
            <div className="modal__title">
              {exported ? `导出「${exported.node_name}」` : "导出失败"}
            </div>

            {exportError && (
              <div className="banner banner--error" role="alert">
                <span>⚠︎</span>
                <div>{exportError}</div>
              </div>
            )}

            {exported && (
              <>
                {/* 二维码是内联 SVG（后端生成），不走 <img src>：
                    离线可用，也没有额外的网络请求。 */}
                <div className="qr" dangerouslySetInnerHTML={{ __html: exported.svg }} />

                {/* 丢失字段必须显示。分享链接的表达能力比内部模型窄，
                    静默丢弃会让目标端行为和本机不同，而用户无从察觉。 */}
                {exported.lost.length > 0 && (
                  <div className="banner banner--warn">
                    <span>⚠︎</span>
                    <div>
                      以下设置无法写进分享链接，扫码后不会生效：
                      <ul style={{ margin: "6px 0 0 16px" }}>
                        {exported.lost.map((x) => (
                          <li key={x}>{x}</li>
                        ))}
                      </ul>
                    </div>
                  </div>
                )}

                <div className="field__hint" style={{ marginTop: 8 }}>
                  链接
                </div>
                <textarea className="input mono" readOnly rows={4} value={exported.uri} />

                <div className="row" style={{ marginTop: 10, gap: 8 }}>
                  {/* task-23 B3：原来是**裸** `navigator.clipboard.writeText`（连 catch
                      都没有）—— 剪贴板被拒时用户以为复制成功、贴出去却是空的。
                      改用与日志页/「报告问题」同一个 `CopyButton`：失败给
                      `role="alert"` + 可手动选中的 textarea。 */}
                  <CopyButton label="复制链接" text={exported.uri} className="btn btn--primary" />
                  <button className="btn" onClick={() => setExported(null)}>
                    关闭
                  </button>
                </div>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function NodeRow({
  node,
  selected,
  probe,
  busy,
  tabIndex,
  onKeyDown,
  onSelect,
  onDelete,
  onExport,
}: {
  node: Node;
  selected: boolean;
  /** 本地 → 服务器的 TCP 握手 RTT（中位数）。这是「延迟」。 */
  /** 这个节点的最近一次探测结果（`snapshot.latency[node.id]`）；`undefined` = 没测过。 */
  probe: ProbeResult | undefined;
  busy: boolean;
  /** roving tabindex：只有一条行是 `0`，其余 `-1`（task-23 B1）。 */
  tabIndex: number;
  onKeyDown: (e: KeyboardEvent<HTMLDivElement>) => void;
  onSelect: () => void;
  onDelete: () => void;
  onExport: () => void;
}) {
  // 不可用时这个徽章**绝不能是绿的**。
  //
  // 它是「本地 → 服务器的 TCP 往返」，只表示**距离**，不代表能用。
  // 实测有节点 TCP 握手 55ms 完全正常、却转发不了任何流量 —— 那时绿色的
  // 「55 ms」紧挨着红色的「不可用」，读起来就是自相矛盾。
  // 所以：不可用时降级成中性色，**并且两个徽章都带上文字标签**
  // （下面 `distanceLabel` / `availabilityLabel`），把语义从悬停提示里
  // 搬到界面上。
  // 四个值**都来自同一个 `probe`**（task-154：原来它们是四个独立 props，容易各自漂移）
  const latencyMs = probe?.server_rtt_ms ?? null;
  const available = probe?.available ?? null;
  const latencyError = probe?.error ?? null;
  const probed = probe !== undefined;
  const tier = available === false ? "unknown" : latencyTier(latencyMs);
  const fromSubscription = node.source.kind === "subscription";

  const distanceLabel = distanceLabelFor(latencyMs, latencyError, probed);
  const distanceTitle = distanceTitleFor(latencyMs, available, latencyError, probed);
  const availabilityLabel = availabilityLabelFor(available);
  const availabilityTitle = availabilityTitleFor(probe);
  const availabilityTone = available === null ? "unknown" : available ? "fast" : "slow";

  return (
    <div
      className={`list__row node-row${selected ? " is-selected" : ""}`}
      // task-23 B1：整行是一个**单选**项。键盘：Enter/Space 选中、方向键组内移动；
      // 读屏：能听到「单选、已选中/未选中」。
      role="radio"
      aria-checked={selected}
      aria-disabled={busy || undefined}
      tabIndex={tabIndex}
      onClick={busy ? undefined : onSelect}
      onKeyDown={onKeyDown}
      // task-120：这里原来是「**正在使用**这个节点」/「当前」。判据是
      // `settings.selected_node`，那是**选中的意图**，不是数据面正在用的出口：
      // 断开后（`runtime.running=false`）它不变；`mode=direct` 时核心不接管流量；
      // 删掉当前节点时后端会把 selected_node 静默改成列表第一个**且不重启核心**
      // （`commands/nodes.rs:239-241`），流量还在被删的那台。
      // 所以只说**确实由这个字段成立**的事：它被选中了。现在时的那半交给
      // 仪表盘（`Dashboard.tsx:148` 用的是 `connected && selected`）。
      /*
        task-128（B2）：`onClick={busy ? undefined : onSelect}`（下面 20 行）——
        **忙的时候点击不触发任何事**，而 `title` 原来还是「点击切换到该节点」：
        测延迟/增删节点/切换正在进行时，行看起来可点、点下去毫无反应。
        文案跟着**同一个判据**（`busy`）走，忙时就说清为什么点不动。
      */
      title={
        busy
          ? "操作进行中，暂时不能切换节点"
          : selected
            ? "已选中：核心运行时流量走这个节点"
            : "点击切换到该节点"
      }
    >
      <div className="list__main">
        <div className="node-row__head">
          <span className="list__name">{node.name}</span>
          {selected && <span className="node-row__tag">已选中</span>}
        </div>
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
      <span className={`badge badge--${tier} node-row__metric`} title={distanceTitle}>
        <span className="node-row__metric-key">距离</span>
        {distanceLabel}
      </span>
      <span
        className={`badge badge--${availabilityTone} node-row__metric`}
        title={availabilityTitle}
      >
        {availabilityLabel}
      </span>

      <span className="node-row__actions">
        <button
          className="btn btn--ghost"
          disabled={busy}
          title="导出为二维码 / 分享链接"
          onClick={(e) => {
            e.stopPropagation();
            onExport();
          }}
        >
          二维码
        </button>
        {/* 删除会**落盘**（后端 `delete_node` → `save_nodes`）且无法撤销，所以要确认。
            订阅带来的节点还要额外说清「它会回来」—— 刷新订阅时后端会先按订阅清空、
            再重新导入（`nodes.rs:246-252`），所以手动删掉的那个下次更新又会出现。 */}
        <InlineConfirm
          label="删除"
          className="btn btn--ghost btn--danger"
          disabled={busy}
          title="删除该节点"
          question={
            fromSubscription
              ? // task-155：原来的「下次更新订阅时**会**重新出现」是**无限定的承诺** ——
                // 后端只在「抓取成功 + 解析成功」时才 retain 掉旧节点再重新导入
                // （`commands/nodes.rs` 的刷新路径）；抓取失败时整段不动，上游把这个节点
                // 删掉也不会再回来。所以只承诺能保证的那件事。
                `删除节点「${node.name}」？它来自订阅：如果下次更新能成功抓取并解析到它，它会重新出现；抓取失败或上游把它删掉就不会。此操作会写入配置文件，无法撤销。`
              : `删除节点「${node.name}」？此操作会写入配置文件，无法撤销。`
          }
          confirmLabel="确认删除"
          onConfirm={onDelete}
        />
      </span>
    </div>
  );
}

/**
 * 四个取值助手。
 *
 * 抽成函数而不是写成嵌套三元（`a ? b : c ? d : e`）：这几个值各自有三个分支，
 * 而且 `available` 是 `boolean | null` 的三态，串在一行里读不出「哪一态对应哪句话」。
 * 分开之后每个判断都能单独读，也方便单测。
 */

/**
 * 距离徽章上的文字。
 *
 * task-120：这里原来只吃 `latencyMs` / `latencyError` **两个**值，而「有没有探测
 * 结果」是第三个独立事实 —— 于是三种不同的情况被压成了两句，正好把注释里声称
 * 「是两回事」的那两件事合并了：
 *
 * | 事实 | 旧文案 | 现在 |
 * |---|---|---|
 * | 没探测过 | 未测 | 未测 |
 * | 探测过、RTT 采样失败（`server_rtt_ms = null, error = null`） | **未测**（错：读成"没测过"） | 距离未知 |
 * | 探测过、明确失败（有 error） | 测不到 | 测不到 |
 *
 * 中间那一态是真实的：后端 `probe.rs` 的成功分支允许 `server_rtt_ms = None`
 * 而 `error = None`（RTT 与可用性互不依赖，且测试
 * `availability_does_not_depend_on_rtt` 明确钉住「RTT 测不到 ≠ 不可用」）。
 * `probed` 就是「`latency[node.id]` 存不存在」，由调用方传入。
 */
function distanceLabelFor(
  latencyMs: number | null,
  latencyError: string | null,
  probed: boolean,
): string {
  if (latencyMs !== null) return `${latencyMs} ms`;
  if (latencyError) return "测不到";
  return probed ? "距离未知" : "未测";
}

/** 距离徽章的悬停说明。不可用时必须讲清「这个数字不代表能用」。 */
function distanceTitleFor(
  latencyMs: number | null,
  available: boolean | null,
  latencyError: string | null,
  probed: boolean,
): string {
  if (available === false) {
    return `到服务器的距离 ${latencyMs ?? "?"} ms —— 但这个节点转发不了流量，这个数字不代表能用`;
  }
  if (latencyMs !== null) {
    return "本地到服务器的 TCP 往返中位数：只表示距离，经节点有没有数据要看右边那个徽章";
  }
  if (latencyError) return latencyError;
  // task-120：原来这里返回**空标题** —— 「测过但量不到距离」这一态在界面上
  // 没有任何解释，用户只能反复重测。这一态与「没测过」必须说清区别。
  return probed ? "这次探测里 3 次 TCP 握手都没成，量不到距离（不影响右边「可用」的判定）" : "";
}

/** 可用性徽章上的文字。`null` 是「还没测」，不能说成「不可用」。 */
function availabilityLabelFor(available: boolean | null): string {
  if (available === null) return "未测";
  return available ? "可用" : "不可用";
}

/**
 * 可用性徽章的悬停说明。
 *
 * task-154（B3）：原来 `available === true` 时只写「经该节点**可以正常**取到数据」——
 * 「正常」没有依据，而且**没有时间限定**：`available` 的判据只是 `probe_one` 拿到了
 * ≥1 字节（`crates/xt-core/src/xray/probe.rs:282-292`），它不看 `http_status`，
 * 而探测结果会一直留在界面上（换网 / 节点被封后徽章不会自己变）。
 * 现在只陈述**这次探测真正测到的东西**：HTTP 状态（有就写）、经节点耗时、以及**测于何时**。
 */
export function availabilityTitleFor(probe: ProbeResult | undefined): string {
  if (!probe) return "尚未测试";
  if (!probe.available) return probe.error ?? "经该节点取不到数据";
  const bits = ["经该节点取到了数据"];
  if (typeof probe.http_status === "number" && probe.http_status > 0) {
    bits.push(`（HTTP ${probe.http_status}）`);
  }
  if (typeof probe.through_node_ms === "number") {
    bits.push(`，经节点耗时 ${probe.through_node_ms} ms`);
  }
  bits.push(` · 测于 ${formatTimestamp(probe.tested_at)}`);
  return bits.join("");
}
