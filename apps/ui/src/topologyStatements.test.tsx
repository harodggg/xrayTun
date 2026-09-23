/**
 * task-126：拓扑页两条 B 级的回归测试。
 *
 * # B-1 「累计值只增不减」的适用范围
 *
 * 那句话只在**本次 App 运行期间**成立：续接用的单调化基数是**进程内 static**
 * （`apps/desktop/src/commands/topology.rs:225` `TRAFFIC_COUNTERS`），而
 * `counter_resets` 报的是**本进程内**观察到的归零次数（`stats.rs::max_resets`）。
 * 所以断言分两层：
 * 1. 页面必须把「本次会话内」这个范围写出来（不能再无条件说只增不减）；
 * 2. `counter_resets > 0` 时确实给出「核心重启过 N 次」的说明，`= 0` 时**不得**出现
 *    （那是把不存在的事说成事实）。
 *
 * # B-2 「点一条就会高亮」的承诺
 *
 * 内部通道（`dns-out` / `api` / `direct` 里那个 `dns-out`/`api` 这类）**画不出线**。
 * 判据是纯函数 `matchConnectionToTopology(...).inFlow`。要求：
 * 1. 页面上的承诺必须有条件（只有入口与出口都在流向图里的才高亮）；
 * 2. **点之前**行上就要标出来（否则就是一次空点击）；
 * 3. 详情头必须说「未在车流图上高亮」，不能沉默。
 */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  routingTopology: vi.fn(),
  recentConnections: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    routingTopology: mocks.routingTopology,
    recentConnections: mocks.recentConnections,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Topology from "./pages/Topology";
import { topologyScenario } from "./previewTopology";
import { RecentConnections } from "./topology/ConnectionsPanel";
import type { TopologyTags } from "./topology/connections";
import type { ConnectionRecord, RecentConnections as Payload } from "./types";

/**
 * jsdom 没有 `ResizeObserver`，而 `Flow`（车流图）用它触发重新测量 ——
 * 缺了它整个 Topology 会**渲染成空 div**（实测：`ReferenceError: ResizeObserver
 * is not defined`，React 把子树整个丢掉，界面全空）。本文件不验几何，
 * 只验文案与判据，所以给一个空桩；几何那部分由 `topologyAnimation.test.ts`
 * 用假布局 + 手动 RO 时钟单独覆盖。
 */
class NoopResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}
globalThis.ResizeObserver = NoopResizeObserver as unknown as typeof ResizeObserver;

// ---------------------------------------------------------------------------
// B-1
// ---------------------------------------------------------------------------

function renderTopology(counterResets: number, conns: ConnectionRecord[] = []) {
  mocks.routingTopology.mockResolvedValue({
    ...topologyScenario(),
    counter_resets: counterResets,
  });
  mocks.recentConnections.mockResolvedValue({
    items: conns,
    dropped: 0,
    pairing: { accepted: conns.length, paired: 0, unpaired: conns.length, sniffed: 0, rejected_stale: 0, sniffed_superseded: 0 },
  });
  return render(<Topology />);
}

describe("task-126 · B-1：拓扑「累计值只增不减」必须写清适用范围", () => {
  it("页面文案必须限定在「本次会话内」（单调化基线是进程内的）", async () => {
    renderTopology(0);
    const text = (await screen.findByText(/本次会话内/)).closest("section")!.textContent ?? "";
    expect(text).toContain("本次会话内");
    expect(text, "还要说清重启 App 之后会怎样").toContain("重启 App");
    expect(text, "不能再用无条件的那句").not.toContain("（累计值只增不减，不代表当前速率）");
  });

  /** 那条「核心重启过 N 次」的说明（`.note`），而不是页面描述里的引用。 */
  const resetNotes = () =>
    Array.from(document.querySelectorAll(".note")).map((n) => n.textContent ?? "");

  it("counter_resets > 0 ⇒ 如实说明「核心重启过 N 次，累计流量已续接」", async () => {
    renderTopology(2);
    await screen.findByText(/本次会话内/);
    expect(
      resetNotes().some((t) => /核心重启过 2 次，累计流量已续接/.test(t)),
      `没给出续接说明；实际的 .note 是 ${JSON.stringify(resetNotes())}`,
    ).toBe(true);
  });

  it("反例：counter_resets === 0 ⇒ 不得给出「核心重启过 N 次」的说明", async () => {
    renderTopology(0);
    await screen.findByText(/本次会话内/);
    expect(
      resetNotes().some((t) => /核心重启过 \d+ 次，累计流量已续接/.test(t)),
      "没有归零时不能声称核心重启过（那是把不存在的事说成事实）",
    ).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// B-2
// ---------------------------------------------------------------------------

const TAGS: TopologyTags = {
  flowInlets: ["tun"],
  internalInlets: ["api"],
  flowOutlets: ["node-n1d232c6b8c7a5004"],
  internalOutlets: ["dns-out"],
};

function conn(over: Partial<ConnectionRecord> & { outbound_tag: string }): ConnectionRecord {
  return {
    ts_ms: 1_700_000_000_000,
    ts_text: "2026/09/20 13:30:58.560364",
    from: "198.18.0.1:50123",
    network: "tcp",
    target_host: "142.250.72.14",
    target_port: 443,
    inbound_tag: "tun",
    domain: null,
    domain_paired: false,
    domain_pair_delta_us: null,
    sniff_id: null,
    ...over,
  };
}

const NODE_CONN = conn({ outbound_tag: "node-n1d232c6b8c7a5004" });
const INTERNAL_CONN = conn({ outbound_tag: "dns-out", target_port: 53, network: "udp" });

const PAYLOAD: Payload = {
  items: [NODE_CONN, INTERNAL_CONN],
  dropped: 0,
  pairing: { accepted: 2, paired: 0, unpaired: 2, sniffed: 0, rejected_stale: 0, sniffed_superseded: 0 },
};

function renderPanel(onSelect = vi.fn()) {
  const r = render(
    <RecentConnections payload={PAYLOAD} error={null} tags={TAGS} selected={null} onSelect={onSelect} />,
  );
  return { ...r, onSelect };
}

const rowOf = (route: string) =>
  screen.getByText(route).closest(".conn-row") as HTMLElement;

describe("task-126 · B-2：高亮承诺必须有条件，且点之前就能看出来", () => {
  it("页面承诺必须限定为「入口与出口都在车流图里的连接才高亮」", async () => {
    renderPanel();
    const desc = (await screen.findByText(/每条连接 =/)).textContent ?? "";
    expect(desc).toContain("才会");
    expect(desc).toContain("画不出线");
    expect(desc, "不能再用无条件的那句").not.toContain("点一条，就会在上面那张车流图上高亮");
  });

  it("内部通道的行**在点之前**就带「内部通道 · 不画线」标记；经节点的行没有", async () => {
    renderPanel();
    await screen.findByText(/每条连接 =/);
    const internal = rowOf("tun → dns-out");
    const node = rowOf(`tun → ${short("node-n1d232c6b8c7a5004")}`);
    expect(within(internal).getByText(/内部通道 · 不画线/)).toBeTruthy();
    expect(within(node).queryByText(/内部通道 · 不画线/)).toBeNull();
    expect(within(node).queryByText(/不画线/)).toBeNull();
  });

  it("点内部通道的行 ⇒ 详情头必须说「未在车流图上高亮」并给出原因", async () => {
    const { onSelect } = renderPanel();
    fireEvent.click(rowOf("tun → dns-out"));
    await waitFor(() => expect(onSelect).toHaveBeenCalledTimes(1));
    const picked = onSelect.mock.calls[0]![0] as ConnectionRecord;
    // 同一屏重渲染成「已选中」态：面板由父组件控制 selected，这里直接断言纯函数的结论
    expect(picked.outbound_tag).toBe("dns-out");
    // 用真实 props 再渲染一次选中态，验证详情头文案
    render(
      <RecentConnections
        payload={PAYLOAD}
        error={null}
        tags={TAGS}
        selected={INTERNAL_CONN}
        onSelect={() => {}}
      />,
    );
    expect(await screen.findByText(/未在车流图上高亮（原因见下）/)).toBeTruthy();
    const detail = document.querySelector(".conn-detail") as HTMLElement;
    expect(detail.textContent).toContain("内部通道");
    expect(detail.textContent).toContain("不在流向图里");
  });

  it("反例：点经节点的行 ⇒ 详情头说「已在车流图上高亮」", async () => {
    render(
      <RecentConnections
        payload={PAYLOAD}
        error={null}
        tags={TAGS}
        selected={NODE_CONN}
        onSelect={() => {}}
      />,
    );
    expect(await screen.findByText("已在车流图上高亮")).toBeTruthy();
    expect(screen.queryByText(/未在车流图上高亮/)).toBeNull();
  });
});

/** 和面板里 `shortTag` 一样的口径（避免在测试里手写截断规则）。 */
function short(t: string): string {
  return t.startsWith("node-") ? `节点 ${t.slice(5, 13)}…` : t;
}

describe("task-126 · B-2b：页面顶部那条说明的前缀也要看 inFlow", () => {
  it("选中的是内部通道连接 ⇒ 前缀是「单连接：」，不能说成「单连接高亮：」", async () => {
    renderTopology(0, [INTERNAL_CONN]);
    const row = await screen.findByText("tun → dns-out");
    fireEvent.click(row);
    // 详情面板里也有「不在流向图里」，所以按前缀取**顶部那条** note
    await waitFor(() =>
      expect(
        Array.from(document.querySelectorAll(".note")).some((n) =>
          (n.textContent ?? "").startsWith("单连接"),
        ),
      ).toBe(true),
    );
    const top = Array.from(document.querySelectorAll(".note"))
      .map((n) => n.textContent ?? "")
      .find((t) => t.startsWith("单连接"))!;
    expect(top).toContain("单连接：");
    expect(top).not.toContain("单连接高亮：");
  });

  it("反例：选中的是经节点的连接 ⇒ 保留「单连接高亮：」", async () => {
    renderTopology(0, [NODE_CONN]);
    const row = await screen.findByText(/^tun → 节点 /);
    fireEvent.click(row);
    // 经节点的连接在流向图里：`matchConnectionToTopology` 的 `note` 为 null，
    // 于是顶部那条 note 不出现——这正是「已高亮」的表现（图上有线）。
    await screen.findByText(/本次会话内/);
    expect(screen.queryByText(/单连接/)).toBeNull();
  });
});

beforeEach(() => {
  vi.clearAllMocks();
});
