/**
 * task-152（A13）：「遗留会话」这一行必须把 `tun_active` 也读进去。
 *
 * # 先核实（只读）——卡里的前提**部分成立**，我按代码事实收紧了判定
 *
 * | 事实 | 依据 |
 * |---|---|
 * | `stale_session` 的判据是「**崩在半路**」（`is_stale()`），一条提交完路由的会话是 `Up`，**不会**进这个字段 | `crates/xt-helper/src/server.rs:267-271` |
 * | `tun_active = state.session.is_some()` —— **正常连接时它也是 true** | `crates/xt-helper/src/server.rs:272-276` |
 * | 启动时的判据 `orphaned = stale_session.is_some() \|\| tun_active` | `apps/desktop/src/lib.rs:242` |
 * | 那个判据的前提：它在 **setup 阶段**跑，**本 App 的核心还没起来**（`:301` 才开始连）；注释原话「helper 里挂着一条**活着的**会话，但这个 App 实例从没连接过」 | `apps/desktop/src/lib.rs:216-238` |
 *
 * ⇒ **结论**：`tun_active && !stale_session` **不等于「遗留」** —— 正常连接（核心在跑）
 * 时它就是本 App 的当前隧道。若照卡面直接报警，会在**每一次正常连接**时误报。
 * 所以判定加一个条件 `!coreRunning`：只有「helper 里有活会话、而本 App 没在跑核心」
 * 才是没人接管的那一态（也与 `lib.rs:242` 在 setup 阶段的结果一致）。
 *
 * # 这一组钉住
 *
 * 四种组合各自的文案（含「无」的两态**措辞不同**，因为一个是真的没有、另一个是
 * 「有会话但归本次运行管」）；**反例**：`tun_active=true` 且核心没跑时，那一行
 * **不得**是「无」；以及**双向敏感性**：把判定改回只读 `stale_session` ⇒ 必须红。
 */
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      saveSettings: mocks.saveSettings,
      start: mocks.start,
      stop: mocks.stop,
    },
    subscribe: () => () => {},
  };
});

import Settings, { legacySessionView } from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

// ---------------------------------------------------------------------------
// 纯函数：四种组合
// ---------------------------------------------------------------------------

describe("task-152 · `legacySessionView` 的四种组合", () => {
  it("有磁盘快照（崩在半路）⇒ 显示 id，并说明它是什么", () => {
    const text = legacySessionView("s1789903818854972000", false, false);
    expect(text).toContain("s1789903818854972000");
    expect(text).toContain("磁盘快照");
    expect(text).toContain("崩在半路");
  });

  it("有活会话 + **核心没在跑** ⇒ 这才是「没人接管」那一态，且给出该做的事", () => {
    const text = legacySessionView(null, true, false);
    expect(text.startsWith("有：")).toBe(true);
    expect(text).toContain("没有在跑核心");
    expect(text).toContain("没人接管");
    expect(text).toContain("修复网络");
    // 不夸大、不承诺
    expect(text).not.toContain("已自动清理");
    expect(text).not.toContain("已回滚");
  });

  it("有活会话 + **核心在跑** ⇒ 就是当前隧道，**不**说成遗留（否则每次正常连接都误报）", () => {
    const text = legacySessionView(null, true, true);
    expect(text).toContain("无");
    expect(text).toContain("由本次运行管理");
    expect(text, "不能把正常连接说成遗留").not.toContain("没人接管");
  });

  it("两者都没有 ⇒ 「无」", () => {
    expect(legacySessionView(null, false, false)).toBe("无");
  });

  it("「无」的两态措辞不同（一个真的没有，一个是归本次运行管）", () => {
    expect(legacySessionView(null, false, false)).not.toBe(legacySessionView(null, true, true));
  });
});

// ---------------------------------------------------------------------------
// 渲染接线：设置页那一行确实走这个判定
// ---------------------------------------------------------------------------

async function renderHelperSection(over: {
  staleSession?: string | null;
  tunActive?: boolean;
  running?: boolean;
}) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({
    ...base,
    runtime: { ...base.runtime, running: over.running ?? false },
    helper: {
      ...base.helper,
      reachable: true,
      socket_present: true,
      stale_session: over.staleSession ?? null,
      tun_active: over.tunActive ?? false,
    },
  } as never);
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Settings focusSection="set-helper" />
    </StoreProvider>,
  );
  await screen.findByText("遗留会话");
}

/** 「遗留会话」那一行的 `<dd>` 文本（用 dt→dd 结构定位，不靠全文匹配）。 */
const legacyCell = () => {
  const dt = screen.getByText("遗留会话");
  return dt.nextElementSibling?.textContent ?? "";
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("task-152 · 设置页「遗留会话」那一行", () => {
  it("`tun_active=true` 且核心没在跑 ⇒ **不得**显示「无」，要显示「有：…没人接管」", async () => {
    await renderHelperSection({ tunActive: true, running: false });
    const cell = legacyCell();
    expect(cell, "这一态正是 A13：界面必须把它说出来").not.toBe("无");
    expect(cell.startsWith("有：")).toBe(true);
    expect(cell).toContain("没人接管");
  });

  it("反例：`tun_active=false` 且无快照 ⇒ 才是「无」", async () => {
    await renderHelperSection({ tunActive: false, running: false });
    expect(legacyCell()).toBe("无");
  });

  it("反例：正常连接中（`tun_active=true` 且核心在跑）⇒ 不报遗留，且说明归属", async () => {
    await renderHelperSection({ tunActive: true, running: true });
    const cell = legacyCell();
    expect(cell).toContain("由本次运行管理");
    expect(cell).not.toContain("没人接管");
  });

  it("有磁盘快照 ⇒ 显示 id（原有行为保留，只是多了说明）", async () => {
    await renderHelperSection({ staleSession: "s1789903818854972000", tunActive: true });
    expect(legacyCell()).toContain("s1789903818854972000");
  });
});
