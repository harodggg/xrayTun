/**
 * task-120：界面「陈述 vs 实现」的回归测试（**只覆盖本轮真的改掉的 A 级**）。
 *
 * # 判据的形状
 *
 * 每条都写成「**某个真实字段为 X ⇒ 文案必须是 Y**」，并配一个**反例**（字段为另一
 * 个值时**不得**出现 Y）。只断言「现在写着某句话」是没意义的 —— 那只是把现有文案
 * 抄进测试；要钉住的是**文案跟着字段走**。
 *
 * 覆盖的 A 级（原始审计清单见 `/tmp/t120-*.md`）：
 *
 * | 位置 | 原来的假陈述 | 现在的判据 |
 * |---|---|---|
 * | `App.tsx` 模式按钮 tooltip | 「只设置系统 HTTP/SOCKS 代理」（全仓 0 命中写入） | 只说本应用不修改系统代理 |
 * | `Dashboard.tsx` 延迟徽章 | `available=false` 也按 `latencyTier` 上绿色 | `available === false` ⇒ 必须写「不可用」 |
 * | `topbarStatus.ts` 徽章 | 「未设系统代理」（对机器状态的断言，读不到） | 只说本应用不设 |
 * | `Subscriptions.tsx` 删除确认 | `sub.node_count`（刷新时写一次的快照值） | 按 `nodes[].source.id` 现数 |
 * | `Logs.tsx` 诊断说明 | 「已抹掉节点地址…可以直接贴到公开 issue」 | 只说真抹了什么 + 要自己核对 |
 * | `Logs.tsx` 空态 | `!running` ⇒ 「核心还没启动过」 | 看 `runtime.started_at_unix` |
 * | `Logs.tsx` 等级计数 | 「错误：1 条」（读成总数） | 说明是**已加载**窗口里的条数 |
 */
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  clearLogs: vi.fn(),
  diagnostics: vi.fn(),
  refreshSubscriptions: vi.fn(),
  removeSubscription: vi.fn(),
  addSubscription: vi.fn(),
  testLatency: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

// `recoveryView` 等纯函数保持真的（用 importOriginal 展开）：被测的是**页面**，
// 不是把整个 ipc 层换成桩。
vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      clearLogs: mocks.clearLogs,
      diagnostics: mocks.diagnostics,
      refreshSubscriptions: mocks.refreshSubscriptions,
      removeSubscription: mocks.removeSubscription,
      addSubscription: mocks.addSubscription,
      testLatency: mocks.testLatency,
      saveSettings: mocks.saveSettings,
      start: mocks.start,
      stop: mocks.stop,
    },
    subscribe: () => () => {},
  };
});

import { TopBar } from "./App";
import Dashboard from "./pages/Dashboard";
import Logs from "./pages/Logs";
import Subscriptions from "./pages/Subscriptions";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import { systemProxyBadge } from "./topbarStatus";

const SELECTED = "n-hk-1";

/** 造快照：`running` / `startedAt` / 选中节点的探测结果都能单独指定。 */
function snap(over: {
  running?: boolean;
  startedAt?: number | null;
  available?: boolean;
  rtt?: number | null;
  mode?: "direct" | "system_proxy" | "tun";
} = {}) {
  const base = scenarioSnapshot();
  const probe = { ...base.latency[SELECTED] };
  if (over.available !== undefined) probe.available = over.available;
  if (over.rtt !== undefined) probe.server_rtt_ms = over.rtt;
  return {
    ...base,
    runtime: {
      ...base.runtime,
      running: over.running ?? true,
      started_at_unix: over.startedAt === undefined ? base.runtime.started_at_unix : over.startedAt,
      routes_committed: true,
    },
    settings: {
      ...base.settings,
      mode: over.mode ?? "tun",
      selected_node: SELECTED,
    },
    latency: { ...base.latency, [SELECTED]: probe },
  } as never;
}

async function renderWith(snapshot: unknown, ui: React.ReactElement, logs: unknown[] = []) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.tailLogs.mockResolvedValue(logs);
  render(<StoreProvider>{ui}</StoreProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.diagnostics.mockResolvedValue("（诊断报告正文）");
  mocks.start.mockResolvedValue(undefined);
  mocks.stop.mockResolvedValue(undefined);
});

describe("task-120 · App 模式按钮的 tooltip 不能承诺产品做不到的事", () => {
  const modeTooltip = async (name: string) => {
    await renderWith(snap(), <TopBar view="dashboard" />);
    const btn = await screen.findByRole("button", { name });
    return btn.getAttribute("title") ?? "";
  };

  it("系统代理模式：必须说「本应用不修改系统代理设置」，不能说「只设置系统 HTTP/SOCKS 代理」", async () => {
    const title = await modeTooltip("系统代理");
    expect(title, "实现从不写系统代理（setwebproxy/scutil 全仓 0 命中），所以不能这么说").not.toContain(
      "只设置系统 HTTP/SOCKS 代理",
    );
    expect(title).toContain("不修改系统代理设置");
    expect(title).toContain("手动");
  });

  it("反例：TUN / 直连 的 tooltip 不受影响（没有被统一改成同一句话）", async () => {
    // 一次 render（`render` 不自动清理，同一测试里多次 render 会叠出多份按钮）
    await renderWith(snap(), <TopBar view="dashboard" />);
    const tip = async (name: string) =>
      (await screen.findByRole("button", { name })).getAttribute("title") ?? "";
    expect(await tip("TUN 模式")).toContain("utun");
    const direct = await tip("直连");
    expect(direct).toBe("不接管任何流量");
    expect(direct).not.toContain("系统代理");
  });
});

describe("task-120 · 仪表盘延迟徽章不能替「可用性」背书", () => {
  it("latency.available === false ⇒ 必须写出「不可用」（不能只给一个数字）", async () => {
    await renderWith(snap({ available: false, rtt: 53 }), <Dashboard onNavigate={() => {}} />);
    const badge = await screen.findByText(/不可用/);
    expect(badge.textContent).toContain("53 ms");
    expect(badge.getAttribute("title")).toContain("取不到数据");
  });

  it("反例：available === true ⇒ 不得出现「不可用」，仍显示延迟数字", async () => {
    await renderWith(snap({ available: true, rtt: 53 }), <Dashboard onNavigate={() => {}} />);
    await screen.findByText("53 ms");
    expect(screen.queryByText(/不可用/)).toBeNull();
  });

  it("反例：量不到距离（rtt=null）且不可用 ⇒ 只说「不可用」，不编数字", async () => {
    await renderWith(snap({ available: false, rtt: null }), <Dashboard onNavigate={() => {}} />);
    const badge = await screen.findByText("不可用");
    expect(badge.textContent).not.toContain("ms");
  });
});

describe("task-120 · 顶栏「系统代理」徽章只陈述本应用这一侧", () => {
  it("不能断言机器的系统代理现状（App 从来不读它）", () => {
    const text = systemProxyBadge("system_proxy", true, 10808) ?? "";
    expect(text).not.toContain("未设系统代理");
    expect(text).toContain("本应用不设系统代理");
    expect(text, "端口要写真的（settings.socks_port）").toContain("127.0.0.1:10808");
  });

  it("反例：非运行 / 非系统代理模式 ⇒ 不出现徽章", () => {
    expect(systemProxyBadge("system_proxy", false, 10808)).toBeNull();
    expect(systemProxyBadge("tun", true, 10808)).toBeNull();
  });

  it("反例：端口读不到时一个数字都不写（不编 10808）", () => {
    const text = systemProxyBadge("system_proxy", true, null) ?? "";
    expect(text).toContain("本地端口");
    expect(text).not.toMatch(/\d{4,5}/);
  });
});

describe("task-120 · 删除订阅的确认语必须用「现在真有几个节点」", () => {
  it("按 nodes[].source.id 现数：预览里 sub-1 的 node_count 是 3，但实际只有 2 个", async () => {
    await renderWith(snap(), <Subscriptions />);
    // 先证明这组数据本身就是矛盾的（node_count 与真实归属不一致）
    const base = scenarioSnapshot();
    const real = base.nodes.filter(
      (n) => n.source.kind === "subscription" && n.source.id === "sub-1",
    ).length;
    const claimed = base.subscriptions.find((s) => s.id === "sub-1")!.node_count;
    expect(real, "预览数据：sub-1 真实 2 个").toBe(2);
    expect(claimed, "预览数据：sub-1 的 node_count 是 3").toBe(3);

    // 三个订阅行都会命中「N 个节点 · 上次成功」，所以先定位到目标行
    await screen.findByText("主订阅 · 机场 A");
    const row = screen.getByText("主订阅 · 机场 A").closest(".list__row") as HTMLElement;
    expect(row.textContent, "列表行上的数字也得是真的").toContain("2 个节点");
    expect(row.textContent).not.toContain("3 个节点");

    fireEvent.click(
      Array.from(row.querySelectorAll("button")).find((b) => b.textContent === "删除")!,
    );
    const question = row.querySelector(".confirm__question")?.textContent ?? "";
    expect(question, "确认语里说的数量必须等于后端即将删掉的数量").toContain("删除它带来的 2 个节点");
    expect(question).not.toContain("3 个节点");
  });
});

describe("task-120 · 日志页的三种「空」与诊断说明", () => {
  it("核心启动过（started_at_unix 非空）+ 现在没在跑 ⇒ **不得**说「核心还没启动过」", async () => {
    await renderWith(snap({ running: false, startedAt: 1_700_000_000 }), <Logs />);
    await screen.findByText(/核心启动过/);
    expect(screen.queryByText(/核心还没启动过/)).toBeNull();
  });

  it("反例：started_at_unix 为 null ⇒ 这才是「核心还没启动过」", async () => {
    await renderWith(snap({ running: false, startedAt: null }), <Logs />);
    await screen.findByText(/核心还没启动过/);
    expect(screen.queryByText(/核心启动过/)).toBeNull();
  });

  it("诊断说明必须如实：节点地址/IP 原样保留，不能说「可以直接贴到公开 issue」", async () => {
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "info", message: "既有日志" },
    ]);
    fireEvent.click(await screen.findByRole("button", { name: "诊断" }));
    const note = await screen.findByText(/已抹掉订阅 URL/);
    const text = note.textContent ?? "";
    expect(text, "只抹 UUID 形状的 token").toContain("UUID");
    expect(text, "IP/域名/IP:port 一律原样保留").toContain("会原样保留");
    expect(text, "必须让人自己核对").toContain("核对");
    expect(text, "不能再说「已抹掉…节点地址」").not.toContain("已抹掉订阅 URL、节点地址");
  });

  it("等级计数必须写明是「已加载的窗口」里的条数，而不是总数", async () => {
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "error", message: "一条错误" },
    ]);
    const btn = await screen.findByRole("button", { name: /错误/ });
    expect(btn.getAttribute("title")).toBe("错误：已加载的 1 条中有 1 条");
  });
});

describe("task-120 · 这些断言不是空壳（判据真的来自字段）", () => {
  it("同一个页面喂两组字段 ⇒ 文案必须不同（防止将来又被写成常量）", async () => {
    mocks.snapshot.mockResolvedValue(snap({ available: true, rtt: 53 }));
    mocks.tailLogs.mockResolvedValue([]);
    const a = render(
      <StoreProvider>
        <Dashboard onNavigate={() => {}} />
      </StoreProvider>,
    );
    await screen.findByText("53 ms");
    const withAvailable = document.querySelector(".dash__status")?.textContent ?? "";
    a.unmount();

    mocks.snapshot.mockResolvedValue(snap({ available: false, rtt: 53 }));
    const b = render(
      <StoreProvider>
        <Dashboard onNavigate={() => {}} />
      </StoreProvider>,
    );
    await screen.findByText(/不可用/);
    const withoutAvailable = document.querySelector(".dash__status")?.textContent ?? "";
    b.unmount();

    expect(withAvailable).not.toBe(withoutAvailable);
    expect(withAvailable).not.toContain("不可用");
    expect(withoutAvailable).toContain("不可用");
  });
});
