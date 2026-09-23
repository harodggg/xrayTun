/**
 * task-154：B 级批次 3（我挑的 5 条）。判据 = **会让用户做错事 / 形成错误信念** 优先。
 *
 * | # | 位置 | 原来的陈述 | 为什么会误导 |
 * |---|---|---|---|
 * | 1 | `Settings.tsx` 助手兼容性横幅 | 「已安装的助手是 X，随 App 附带的是 Y —— **两者不一致**」 | 判据是**协议号**（`commands/helper.rs`），`mismatch` **包含「包版本相同」**的情形 ⇒ 会出现「0.8.33 与 0.8.33 两者不一致」的自相矛盾，用户会怀疑产品胡说或白重装一次特权助手 |
 * | 2 | `Settings.tsx` 核心与数据更新 | `core_version ?? "未找到"` | `core_version` 来自 `core.version`（`snapshot.rs:587`）= `xray version` 第一行；核心**在**但这条命令没给出可解析输出时也是 null ⇒ 同一页「内核」节显示路径、这里说「未找到」 |
 * | 3 | `Nodes.tsx` 可用性徽章 | 「经该节点**可以正常**取到数据」 | `available` 的判据只是 `probe_one` 拿到 ≥1 字节（`probe.rs:282-292`）：**不看 `http_status`、也没有时间限定**（结果会一直留着） |
 * | 4 | `Subscriptions.tsx` 刷新失败提示 | 「已有节点**仍然可用**，可以稍后重试」 | 后端只保证「节点还在列表里」（`nodes.rs` 不动 `i.nodes`），没有任何可用性探测 |
 * | 5 | `Globe.tsx` 飞机数量 | `bytes <= 0 ⇒ 1 架` | `traffic_ok === false` 时 `bytes` 是**占位 0**，而 0 字节也返回 1 ⇒ 读不到流量时**凭空画一架在飞的飞机**（与同页「读不到就不写 0 B」的口径相反） |
 */
import { render, screen, within } from "@testing-library/react";
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

import { vehicleCount } from "./pages/Globe";
import Nodes from "./pages/Nodes";
import Settings from "./pages/Settings";
import Subscriptions from "./pages/Subscriptions";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

async function mount(snapshot: unknown, ui: React.ReactElement, anchor: RegExp | string) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.tailLogs.mockResolvedValue([]);
  render(<StoreProvider>{ui}</StoreProvider>);
  await screen.findByText(anchor as never);
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

// ---------------------------------------------------------------------------
// 1) B5：兼容性判据是协议号，不是「版本不一致」
// ---------------------------------------------------------------------------

describe("task-154 · B5：助手兼容性横幅不许出现「同版本却说两者不一致」", () => {
  const withCheck = (installed: string, bundled: string) => {
    const base = scenarioSnapshot();
    return {
      ...base,
      helper: {
        ...base.helper,
        state: "ready",
        socket_present: true,
        reachable: true,
        version: installed,
        version_check: { state: "mismatch", installed, bundled },
      },
    } as never;
  };

  it("两边版本号**相同**（协议不同）⇒ 必须说清判据是协议号，且不写「两者不一致」", async () => {
    const text = await mount(
      withCheck("0.8.33", "0.8.33"),
      <Settings focusSection="set-helper" />,
      "兼容性检查没通过",
    );
    expect(text).toContain("协议号");
    expect(text).toContain("版本号相同也可能不兼容");
    expect(text, "不许对两个版本号的关系下结论").not.toContain("两者不一致");
    // 该说的后果还在（不许因为改口径把该做的动作删掉）
    expect(text).toContain("助手侧的这部分修复不会生效");
  });

  it("反例：两边版本号不同 ⇒ 同样是「兼容性检查没通过」的口径（判据一致，不给两种说法）", async () => {
    const text = await mount(
      withCheck("0.8.31", "0.8.32"),
      <Settings focusSection="set-helper" />,
      "兼容性检查没通过",
    );
    expect(text).toContain("兼容性检查没通过");
    expect(text).not.toContain("两者不一致");
    expect(text).toContain("0.8.31");
    expect(text).toContain("0.8.32");
  });
});

// ---------------------------------------------------------------------------
// 2) B9：核心在、版本读不出 ≠ 未找到
// ---------------------------------------------------------------------------

describe("task-154 · B9：核心版本读不出来时不许说「未找到」", () => {
  const withCore = (path: string | null, version: string | null) => {
    const base = scenarioSnapshot();
    return {
      ...base,
      core: { ...base.core, path, version },
      update: { ...base.update, core_version: version },
    } as never;
  };

  it("`core.path` 有值 + 版本 null ⇒ 「读不到版本」，不说「未找到核心」", async () => {
    await mount(
      withCore("/Applications/XrayTun.app/Contents/Resources/xray", null),
      <Settings focusSection="set-update" />,
      "核心与数据更新",
    );
    const section = document.querySelector("#set-update") as HTMLElement;
    const text = section.textContent ?? "";
    expect(text).toContain("读不到版本");
    expect(text, "核心明明在，不能说没找到").not.toContain("未找到核心");
  });

  it("反例：`core.path` 为 null ⇒ 才是「未找到核心」", async () => {
    await mount(
      withCore(null, null),
      <Settings focusSection="set-update" />,
      "核心与数据更新",
    );
    const text = (document.querySelector("#set-update") as HTMLElement).textContent ?? "";
    expect(text).toContain("未找到核心");
    expect(text).not.toContain("读不到版本");
  });

  it("反例：版本读得到 ⇒ 直接显示版本号（不画蛇添足）", async () => {
    await mount(
      withCore("/x/bin/xray", "Xray 26.9.9"),
      <Settings focusSection="set-update" />,
      "核心与数据更新",
    );
    const text = (document.querySelector("#set-update") as HTMLElement).textContent ?? "";
    expect(text).toContain("Xray 26.9.9");
    expect(text).not.toContain("读不到版本");
  });
});

// ---------------------------------------------------------------------------
// 3) B3：可用性说明只陈述这次探测真正测到的东西
// ---------------------------------------------------------------------------

describe("task-154 · B3：可用性徽章必须给出测量细节与时间", () => {
  const withProbe = (probe: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return {
      ...base,
      latency: { ...base.latency, "n-hk-1": probe },
    } as never;
  };

  it("可用 ⇒ 带上 HTTP 状态、经节点耗时与**测于何时**", async () => {
    await mount(
      withProbe({
        node_id: "n-hk-1",
        node_name: "香港 · REALITY 01",
        server_rtt_ms: 53,
        available: true,
        through_node_ms: 180,
        http_status: 204,
        error: null,
        tested_at: 1_700_000_000,
      }),
      <Nodes />,
      "香港 · REALITY 01",
    );
    const row = screen.getByText("香港 · REALITY 01").closest(".node-row") as HTMLElement;
    const badge = within(row).getByText("可用");
    const title = badge.getAttribute("title") ?? "";
    expect(title).toContain("HTTP 204");
    expect(title).toContain("经节点耗时 180 ms");
    expect(title, "必须给时间限定（结果会一直留着）").toContain("测于");
    expect(title, "不再拿「正常」这种没依据的词").not.toContain("正常");
  });

  it("反例：不可用 ⇒ 仍然是后端给的原因（没有被改口径）", async () => {
    await mount(
      withProbe({
        node_id: "n-hk-1",
        node_name: "香港 · REALITY 01",
        server_rtt_ms: 53,
        available: false,
        through_node_ms: null,
        http_status: null,
        error: "探针超时",
        tested_at: 1_700_000_000,
      }),
      <Nodes />,
      "香港 · REALITY 01",
    );
    const row = screen.getByText("香港 · REALITY 01").closest(".node-row") as HTMLElement;
    expect(within(row).getByText("不可用").getAttribute("title")).toBe("探针超时");
  });
});

// ---------------------------------------------------------------------------
// 4) B7：刷新失败时不许断言「仍然可用」
// ---------------------------------------------------------------------------

describe("task-154 · B7：订阅刷新失败只能保证「还在列表里」", () => {
  it("有节点 + 上次失败 ⇒ 说「仍保留在列表里」，不说「仍然可用」", async () => {
    const base = scenarioSnapshot();
    const snap = {
      ...base,
      nodes: [
        { ...base.nodes[0]!, id: "n-sub-1", name: "节点 1", source: { kind: "subscription", id: "sub-1" } },
      ],
      subscriptions: [
        {
          id: "sub-1",
          name: "主订阅 · 机场 A",
          url: "https://example.com/sub",
          enabled: true,
          update_interval_hours: 24,
          last_updated: 1_700_000_000,
          last_error: "HTTP 403：订阅 token 可能已过期",
          node_count: 1,
          usage: null,
        },
      ],
    } as never;
    await mount(snap, <Subscriptions />, "主订阅 · 机场 A");
    const row = screen.getByText("主订阅 · 机场 A").closest(".list__row") as HTMLElement;
    const text = row.textContent ?? "";
    expect(text).toContain("仍保留在列表里");
    expect(text, "没有探测支撑就不能说「可用」").not.toContain("仍然可用");
  });
});

// ---------------------------------------------------------------------------
// 5) B2：0 字节 / 读不到流量 ⇒ 不画飞机
// ---------------------------------------------------------------------------

describe("task-154 · B2：飞机数量不能把「没有流量」画成有", () => {
  it("读得到流量 ⇒ 按 512KB 一架，最多 6 架", () => {
    expect(vehicleCount(2 * 1024 * 1024, true)).toBe(4);
    expect(vehicleCount(10 * 1024 * 1024, true)).toBe(6);
    // 有正流量但不足一架 ⇒ 至少一架（这是真的在走）
    expect(vehicleCount(1024, true)).toBe(1);
  });

  it("**读不到流量**（`traffic_ok === false`，`bytes` 是占位 0）⇒ 0 架", () => {
    expect(vehicleCount(0, false)).toBe(0);
    // 即使 bytes 因为别的原因非 0，读不到就不画
    expect(vehicleCount(8 * 1024 * 1024, false)).toBe(0);
  });

  it("真的是 0 字节（读得到，但还没流量）⇒ 也 0 架（原来会画 1 架）", () => {
    expect(vehicleCount(0, true)).toBe(0);
  });
});
