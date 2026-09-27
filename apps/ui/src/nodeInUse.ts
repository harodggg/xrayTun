/**
 * 「**实际在用**的节点」与「**你选的**节点」—— 两个事实的**唯一真源**（纯函数）。
 *
 * # 为什么要有这个文件（用户原话）
 *
 * > 「切换节点，没用，没有切换到香港，还是在美国」
 *
 * 真实情况：他选中的**香港**节点当时不可用（`egress-broken`：TCP 能连、经它的
 * 真实请求拿不到响应），App 的回落策略**自动换到另一个可用的美国节点**继续连接，
 * 并且按既有约定**不改用户选中的项**。行为是对的 —— 界面过去只显示「选中项」，
 * 于是「你选的」被当成了「正在用的」。
 *
 * # 判据只来自后端字段（不许猜）
 *
 * * **实际在用** = `snapshot.active_node`（`apps/desktop/src/state.rs` 的
 *   `Inner::active_node` → `AppSnapshot.active_node`；写入点
 *   `apps/desktop/src/commands/core.rs` 启动成功那一处）；
 * * **你选的** = `snapshot.settings.selected_node`（回落**从不写回**它）；
 * * **为什么换** = `snapshot.node_health[你选的].{class,label,advice}`
 *   （节点尝试账，写入点 `commands/core.rs::note_node_failure`）。
 *
 * **不解析 `notice` 文案、不按时间猜、不把 `last_good_node` 当「正在用的」**
 * （`last_good_node` 的语义是「上一次**验证过**能用」，在启动与验证之间它可能是
 * 上一场的节点 —— 拿它说「实际在用」就是另一个假陈述）。
 *
 * # 什么时候**不**显示（防狼来了）
 *
 * 只有「核心在跑 + 后端给了实际在用的节点 + 它与选中项**不同**」才成立。
 * 其余一律返回 `null`：没有回落却说「实际在用 X」，就是与选中项重复的噪声。
 */
import { stripMarkup } from "./failure";
import type { AppSnapshot, Node, NodeHealthRecord } from "./types";

/** 一个节点在界面上的最小事实（来自快照的 `nodes`，找不到时不编地址）。 */
export interface NodeFact {
  id: string;
  name: string;
  /** 它还在 `snapshot.nodes` 里吗（被删掉的节点只有 id 可写）。 */
  known: boolean;
  /** `「名字」（地址:端口）`；不在列表里时为 `「id」（已不在节点列表）`。 */
  label: string;
}

/** 「实际在用 vs 你选的」视图；`null` = **没有回落**，界面不许显示这类信息。 */
export interface InUseView {
  /** **实际在用**（数据面事实）。 */
  used: NodeFact;
  /** **你选的**（用户意图；回落时它**没有被改动**）。 */
  selected: NodeFact;
  /** 你选的那台为什么没被用（后端账本；**没有账本时为 `null`，不许编类别**）。 */
  reason: NodeHealthRecord | null;
  /** 一行、两个事实都在：`实际在用：X（你选的：Y）`。 */
  headline: string;
  /** 展开的说明：本次用了哪个 / 为什么换 / **你的选择没有被改动**。 */
  detail: string;
}

/** 按 id 取节点事实；`id` 为空时返回 `null`。 */
export function nodeFact(nodes: Node[], id: string | null | undefined): NodeFact | null {
  if (!id) return null;
  const node = nodes.find((n) => n.id === id);
  if (!node) {
    // **被删掉的节点也不编名字/地址**：只有 id 是可核实的。
    return { id, name: id, known: false, label: `「${id}」（已不在节点列表）` };
  }
  return {
    id: node.id,
    name: node.name,
    known: true,
    label: `「${node.name}」（${node.address}:${node.port}）`,
  };
}

/**
 * 某个节点最近一次尝试失败的账本。
 *
 * **空/缺字段一律返回 `null`**：界面的全部判据都必须能失败在「有账本」上，
 * 不许把「没读到」当成「没失败过」或相反。
 */
export function healthOf(
  snapshot: AppSnapshot,
  id: string | null | undefined,
): NodeHealthRecord | null {
  if (!id) return null;
  const table = snapshot.node_health;
  if (!table || typeof table !== "object") return null;
  return table[id] ?? null;
}

/**
 * 组装「实际在用 vs 你选的」视图；**没有回落时返回 `null`**。
 *
 * 四个前置条件缺一不可：核心在跑、后端给了 `active_node`、用户有选中项、
 * 且两者**不同**。少任何一个都返回 `null`（宁可不说，也不许说错）。
 */
export function inUseView(snapshot: AppSnapshot): InUseView | null {
  if (!snapshot || !snapshot.runtime?.running) return null;
  const usedId = snapshot.active_node ?? null;
  const selectedId = snapshot.settings?.selected_node ?? null;
  if (!usedId || !selectedId) return null;
  if (usedId === selectedId) return null;

  const used = nodeFact(snapshot.nodes ?? [], usedId);
  const selected = nodeFact(snapshot.nodes ?? [], selectedId);
  if (!used || !selected) return null;

  const reason = healthOf(snapshot, selectedId);
  // 后端原文是带 `**` 的工程散文（`node_health.rs` / `supervisor.rs`），
  // 进界面前统一去记号（与 `failure.ts` 同族口径）。
  const why = reason
    ? `${stripMarkup(reason.label)}（${stripMarkup(reason.class)}）`
    : "本次不可用（原因见日志）";

  return {
    used,
    selected,
    reason,
    headline: `实际在用：${used.name}（你选的：${selected.name}）`,
    detail:
      `本次实际在用${used.label}；你选的${selected.label}${why} ⇒ App 自动回落到另一个节点继续连接。` +
      `你的选择没有被改动：设置里仍然是${selected.label}，节点列表里它仍标着「已选中」` +
      `（并带有本次失败的类别与时间）。`,
  };
}

/** 节点列表里那一行失败标记的文案（`null` = 这个节点没有失败账本，不显示）。 */
export function healthBadge(record: NodeHealthRecord | null | undefined): {
  text: string;
  title: string;
} | null {
  if (!record) return null;
  const who = stripMarkup(record.label);
  const slug = stripMarkup(record.class);
  const advice = stripMarkup(record.advice);
  return {
    // 类别**中英文都给**：中文是给用户的，slug 是给用户贴给开发者的（也是判据锚点）。
    text: `上次失败：${who}（${slug}）`,
    title:
      `最近一次启动尝试失败：${who}（${slug}），连续失败 ${record.failures} 次。` +
      (advice ? `下一步：${advice}。` : "") +
      (stripMarkup(record.detail) ? `原文：${stripMarkup(record.detail)}` : ""),
  };
}
