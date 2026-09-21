/**
 * task-68 ②：仪表盘「立即修复」必须是**二次确认**，且问句写清真实后果。
 *
 * # 防的故障
 *
 * `Dashboard.tsx` 里那条「检测到上次异常退出遗留的网络配置…建议立即回滚」的横幅，
 * 右侧的「立即修复」以前是**点一下就执行** `api.restoreStale()`。而这个动作在 helper
 * 侧是 `Request::Restore`：**无条件**强清理（先拆内存里的活会话、关掉 utun fd，
 * 再按磁盘快照还原路由与 DNS）——也就是说，正连着的时候点它，隧道会被拆掉。
 *
 * # 为什么是「二次确认」而不是「撤销」
 *
 * 与 `InlineConfirm` 的既有三处同源：这是改**系统网络配置**的动作，不做就没有；
 * 做了也没有「还原回去」的 API（`restore_stale` 只发 Restore + 重建快照）。
 * 所以只能让用户在点之前读一句后果。
 *
 * # 断言方式
 *
 * `NoticeAction` / `collectNotices` 都是纯的（前者只吃 props，后者只吃快照），
 * 所以**不需要**渲染整个仪表盘、也不需要 StoreProvider —— 这条测试的重点是
 * 「未确认时后端 API 不被调用」，不是布局。
 */

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { collectNotices, NoticeAction } from "./pages/Dashboard";

/** 与 `Dashboard.tsx` 里那条 stale notice 完全一致的动作（问句见下）。 */
function restoreAction(run: () => void) {
  return {
    label: "立即修复",
    run,
    confirm: {
      question:
        "回滚网络配置会还原 helper 装的路由与 DNS，并拆掉当前正在生效的那条隧道（utun 网卡也会移除）——网络会回到直连；如果你正连着，连接会断。",
      confirmLabel: "确认回滚",
    },
  };
}

describe("NoticeAction：带 confirm 的动作走内联二次确认", () => {
  it("未确认时后端 API **不得**被调用，且问句里含具体后果", () => {
    const run = vi.fn();
    render(<NoticeAction action={restoreAction(run)} />);

    // 只有那颗原按钮。
    const label = screen.getByText("立即修复");
    expect(run).not.toHaveBeenCalled();

    fireEvent.click(label);

    // 出现问句，且问句说的是**真实后果**（不是「确定吗？」）。
    expect(screen.getByText(/回滚网络配置会还原 helper 装的路由与 DNS/)).toBeTruthy();
    expect(screen.getByText(/隧道/)).toBeTruthy();
    expect(screen.getByText(/网络会回到直连/)).toBeTruthy();
    expect(screen.getByText(/连接会断/)).toBeTruthy();
    // **不许**写代码确认不了的后果：`restore_stale` 确实不重连，但隧道被拆之后
    // 看门狗仍可能把隧道重建回来（`was_connected` 未被清），所以「不会自动重连」
    // 这句话的真假取决于后续行为，不能写进问句。
    expect(screen.queryByText(/不会自动重连/)).toBeNull();
    // 仍然**没有**执行。
    expect(run).not.toHaveBeenCalled();
  });

  it("点「确认回滚」→ 恰好执行一次", () => {
    const run = vi.fn();
    render(<NoticeAction action={restoreAction(run)} />);
    fireEvent.click(screen.getByText("立即修复"));
    fireEvent.click(screen.getByText("确认回滚"));
    expect(run).toHaveBeenCalledTimes(1);
  });

  it("点「取消」→ 不执行", () => {
    const run = vi.fn();
    render(<NoticeAction action={restoreAction(run)} />);
    fireEvent.click(screen.getByText("立即修复"));
    fireEvent.click(screen.getByText("取消"));
    expect(run).not.toHaveBeenCalled();
    // 收起确认态，回到原按钮。
    expect(screen.getByText("立即修复")).toBeTruthy();
    expect(screen.queryByText("确认回滚")).toBeNull();
  });

  it("按 Esc → 不执行（确认态必须有键盘退出路径）", () => {
    const run = vi.fn();
    render(<NoticeAction action={restoreAction(run)} />);
    fireEvent.click(screen.getByText("立即修复"));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(run).not.toHaveBeenCalled();
    expect(screen.queryByText("确认回滚")).toBeNull();
  });

  it("没有 confirm 的动作（例如「去处理」只是导航）**保持**一次点击", () => {
    const run = vi.fn();
    render(<NoticeAction action={{ label: "去处理", run }} />);
    fireEvent.click(screen.getByText("去处理"));
    expect(run).toHaveBeenCalledTimes(1);
  });
});

/** 只造 `collectNotices` 真正会读的字段。 */
function snap(extra: Record<string, unknown>) {
  return {
    runtime: { running: true, last_error: null, recovery: null },
    core: { path: "/xray", supports_native_tun: true, min_native_tun_version: "26.1.18" },
    helper: { stale_session: null, socket_present: true },
    notice: null,
    ...extra,
  } as never;
}

describe("collectNotices：哪条 notice 该带确认，由代码而不是运气决定", () => {
  const run = (() => Promise.resolve(true)) as never;
  const nav = () => {};

  it("「检测到遗留网络配置」的那条**必须**带 confirm，且问句含真实后果", () => {
    const notices = collectNotices(snap({ helper: { stale_session: "sess-7f3a91", socket_present: true } }), run, nav);
    const stale = notices.find((n) => n.key === "stale");
    expect(stale).toBeTruthy();
    expect(stale!.action?.label).toBe("立即修复");
    const q = stale!.action?.confirm?.question ?? "";
    // 断言具体后果，而不是只断言「出现了问句」。
    expect(q).toContain("路由与 DNS");
    expect(q).toContain("隧道");
    expect(q).toContain("回到直连");
    expect(q).toContain("连接会断");
    expect(stale!.action?.confirm?.confirmLabel).toBe("确认回滚");
  });

  it("「去处理」那条只是导航 → **不加**确认（避免变成纯摩擦）", () => {
    const notices = collectNotices(snap({ notice: "helper 未安装：TUN 模式需要它。" }), run, nav);
    const n = notices.find((x) => x.key === "notice");
    expect(n).toBeTruthy();
    expect(n!.action?.label).toBe("去处理");
    expect(n!.action?.confirm).toBeUndefined();
  });
});
