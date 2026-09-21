/**
 * task-72：同一屏上只能有**一个**「连接/断开」控件；「未设系统代理」徽章只在
 * 「核心运行中 + 系统代理模式」出现。
 *
 * # 防的两个故障
 *
 * 1. **同屏两个「断开」**（product-manager 的 task-59 实测）：顶栏一颗、仪表盘状态区
 *    一颗。收敛的**前提**是两者调同一个命令 —— 读码确认：顶栏 `toggleRun()` 与
 *    仪表盘那颗都是 `run("stop"|"start", api.stop|api.start)`，逐字相同，
 *    所以删掉仪表盘那颗不丢功能。本文件同时钉住「删掉之后功能还在」：
 *    点那一颗必须仍然调用 `stop`。
 * 2. **「未设系统代理」徽章常驻**：`SystemProxy` 是 `#[default]`，新用户一打开就是
 *    它，而那一刻没有端口需要指向 ⇒ 纯噪音。现在只在运行时出现；但**信息本身
 *    不许消失**（仪表盘 `sub` 与 live region 里那句不受此条件影响，另有测试）。
 *
 * # 为什么渲染 TopBar + Dashboard 两个组件
 *
 * 「同屏只有一个」是一个**跨组件**的性质：分别断言两个组件各自的按钮数，
 * 就漏掉了「两边各一颗」正是要防的那种情况。
 */

import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  stop: vi.fn(),
  start: vi.fn(),
  tailLogs: vi.fn(),
}));

vi.mock("./ipc", async () => {
  const actual = await vi.importActual<typeof import("./ipc")>("./ipc");
  return {
    ...actual,
    // store 在挂载时会调它；jsdom 里没有 Tauri 事件桥。
    subscribe: () => () => {},
    api: { ...actual.api, snapshot: mocks.snapshot, stop: mocks.stop, start: mocks.start, tailLogs: mocks.tailLogs },
  };
});

import { TopBar } from "./App";
import Dashboard from "./pages/Dashboard";
import { StoreProvider } from "./store";

// 每个用例只清**调用记录**（`clearAllMocks` 保留实现）——否则上一条用例
// 点过的 `stop` 会漏进下一条的 `not.toHaveBeenCalled()`。
beforeEach(() => {
  vi.clearAllMocks();
});

function snap(over: Record<string, unknown> = {}) {
  return {
    app_version: "0.8.0",
    settings: {
      mode: "tun",
      socks_port: 10808,
      http_port: 10809,
      selected_node: "n1",
      routing_preset: "bypass_mainland",
      custom_rules: [],
      show_speed_in_title: true,
    },
    runtime: {
      running: true,
      routes_committed: true,
      pid: 1,
      started_at_unix: null,
      tun_interface: "utun3",
      config_path: null,
      last_error: null,
      last_good_node: null,
      recovery: {
        recovering: false,
        attempt: 0,
        probe_failures: 0,
        started_unix: null,
        last_outcome: null,
        finished_unix: null,
      },
    },
    core: {
      path: "/xray",
      version: "26.9.9",
      supports_native_tun: true,
      min_native_tun_version: "26.1.18",
      error: null,
    },
    helper: {
      socket_present: true,
      reachable: true,
      version: "0.8.0",
      protocol: 1,
      tun_active: true,
      stale_session: null,
      needs_approval: false,
      state: "ready",
      error: null,
    },
    notice: null,
    nodes: [{ id: "n1", name: "香港 · REALITY 01", source: { kind: "manual" } }],
    latency: {},
    subscriptions: [],
    traffic: { rx_bytes: 1, tx_bytes: 1, rx_rate: 0, tx_rate: 0 },
    ...over,
  };
}

function renderScreen(s: unknown) {
  mocks.snapshot.mockResolvedValue(s);
  // store 挂载时会拉历史日志；不返回数组会让它 `entries.map` 炸掉（与本卡无关，但会让全部用例红）。
  mocks.tailLogs.mockResolvedValue([]);
  return render(
    <StoreProvider>
      <TopBar view="dashboard" />
      <Dashboard onNavigate={() => {}} />
    </StoreProvider>,
  );
}

/** 全屏「同一个可访问名」的按钮。 */
function buttonsNamed(name: string) {
  return screen
    .getAllByRole("button")
    .filter((b) => (b.textContent ?? "").trim() === name);
}

describe("task-72 ②：「连接/断开」全屏只有一个", () => {
  it("已连接（TUN）→ 全屏恰好一个「断开」，且点它真的调 stop", async () => {
    renderScreen(snap());
    await waitFor(() => expect(screen.getByText("已连接")).toBeTruthy());

    expect(buttonsNamed("断开")).toHaveLength(1);
    // 仪表盘状态区里**不再有**等价按钮（收敛前这里有第二颗）。
    const dashActions = document.querySelector<HTMLElement>(".dash__actions");
    expect(dashActions).not.toBeNull();
    expect(
      [...(dashActions?.querySelectorAll("button") ?? [])].filter((b) =>
        ["连接", "断开"].includes((b.textContent ?? "").trim()),
      ),
    ).toHaveLength(0);
    // 但仪表盘那一行**不是空的**（收敛不能把整行动作删掉）。
    expect(dashActions?.querySelectorAll("button").length ?? 0).toBeGreaterThan(0);

    fireEvent.click(buttonsNamed("断开")[0]!);
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledTimes(1));
    expect(mocks.start).not.toHaveBeenCalled();
  });

  it("未连接 → 全屏恰好一个「连接」，且点它真的调 start", async () => {
    renderScreen(
      snap({
        runtime: { ...snap().runtime, running: false, routes_committed: false },
      }),
    );
    await waitFor(() => expect(screen.getByText("未连接")).toBeTruthy());

    expect(buttonsNamed("连接")).toHaveLength(1);
    expect(buttonsNamed("断开")).toHaveLength(0);

    fireEvent.click(buttonsNamed("连接")[0]!);
    await waitFor(() => expect(mocks.start).toHaveBeenCalledTimes(1));
    expect(mocks.stop).not.toHaveBeenCalled();
  });
});

describe("task-72 ①：「未设系统代理」徽章三态", () => {
  const badge = () => screen.queryByText(/未设系统代理/);

  it("未运行（默认模式 = 系统代理）→ **不出现**徽章", async () => {
    renderScreen(
      snap({
        settings: { ...snap().settings, mode: "system_proxy" },
        runtime: { ...snap().runtime, running: false, routes_committed: false },
      }),
    );
    await waitFor(() => expect(screen.getByText("未连接")).toBeTruthy());
    expect(badge()).toBeNull();
  });

  it("运行中 + 系统代理 → 出现，且带真实端口", async () => {
    renderScreen(
      snap({
        settings: { ...snap().settings, mode: "system_proxy" },
        runtime: { ...snap().runtime, running: true, routes_committed: false },
      }),
    );
    await waitFor(() => expect(badge()).toBeTruthy());
    expect(badge()!.textContent).toContain("127.0.0.1:10808");
  });

  it("运行中 + TUN → 不出现（那句提示只属于系统代理模式）", async () => {
    renderScreen(snap());
    await waitFor(() => expect(screen.getByText("已连接")).toBeTruthy());
    expect(badge()).toBeNull();
  });

  it("运行中 + 系统代理 → 徽章**与**仪表盘 sub 同时出现（徽章条件只作用于徽章）", async () => {
    // 防的是「为了不吵，把信息一起删掉」：`systemProxyBadge()` 只管徽章，
    // `appStatus` 的第 7 个分支照旧把同一句话写进 sub / detail（live region）。
    renderScreen(
      snap({
        settings: { ...snap().settings, mode: "system_proxy" },
        runtime: { ...snap().runtime, running: true, routes_committed: false },
      }),
    );
    await waitFor(() => expect(badge()).toBeTruthy());
    // 同一句话在**两个地方**都要有：仪表盘状态区的 sub，以及顶栏 `.sr-only`
    // 的 live region（`status.detail`）。所以按容器取，而不是 `getByText`
    // （后者会因为命中两处而报「找到多个元素」——那本身也证明了「两处都在」）。
    const sub = document.querySelector<HTMLElement>(".dash__state-sub");
    expect(sub?.textContent).toContain("系统代理未被本应用修改");
    const live = document.querySelector<HTMLElement>(".topbar .sr-only");
    expect(live?.textContent).toContain("系统代理未被本应用修改");
    // 端口读得到时：徽章与 sub 都写具体值。
    expect(badge()!.textContent).toContain("127.0.0.1:10808");
    expect(sub?.textContent).toContain("127.0.0.1:10808");
  });
});
