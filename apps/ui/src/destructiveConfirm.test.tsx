/**
 * 破坏性操作必须**先确认**（task-23 缺陷 B）。
 *
 * # 背景（实测）
 *
 * 节点删除、订阅删除、日志「清空」以前都是**点一下就执行**；全仓库 `window.confirm`
 * 0 命中。其中两处后果比「少了一行」重得多：
 *   · 删订阅会**连带删掉它带来的全部节点**（`commands/nodes.rs` 的 `nodes.retain(...)`）；
 *   · 清空日志会**删除日志文件本身**（`xt-core/src/store.rs` 的 `clear_logs` +
 *     测试 `clear_logs_removes_files_too`），服务端不留副本 —— 所以只能给确认，
 *     给不了撤销。
 *
 * # 覆盖
 *
 * 1. 组件级：`InlineConfirm` 的四种行为（未确认不执行 / 确认才执行 / 取消 / Esc）；
 * 2. 三处真实站点（日志清空、节点删除、订阅删除）**都**先确认再调用后端 ——
 *    这是防「只在某一处接上、另两处忘了」的回归。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ReactElement } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  clearLogs: vi.fn(),
  deleteNode: vi.fn(),
  removeSubscription: vi.fn(),
  selectNode: vi.fn(),
  addSubscription: vi.fn(),
  refreshSubscriptions: vi.fn(),
  testLatency: vi.fn(),
  exportNode: vi.fn(),
  addNode: vi.fn(),
  diagnostics: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    clearLogs: mocks.clearLogs,
    deleteNode: mocks.deleteNode,
    removeSubscription: mocks.removeSubscription,
    selectNode: mocks.selectNode,
    addSubscription: mocks.addSubscription,
    refreshSubscriptions: mocks.refreshSubscriptions,
    testLatency: mocks.testLatency,
    exportNode: mocks.exportNode,
    addNode: mocks.addNode,
    diagnostics: mocks.diagnostics,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import { InlineConfirm } from "./InlineConfirm";
import Logs from "./pages/Logs";
import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import { StoreProvider } from "./store";

/**
 * 页面只用快照的少数几个字段；这里按用到的字段造最小快照，其余缺失字段
 * 与本组断言无关。**用 `as unknown as` 而不是把类型放宽** —— 免得为了测试
 * 把生产类型改成可选。
 */
function snapWith(extra: Record<string, unknown>) {
  return {
    runtime: { running: true, last_error: null, recovery: null },
    // 页面各取所需；给全最小骨架，免得某个页面读 `settings.selected_node` 时炸掉
    // （那会让「测试挂了」看起来像产品缺陷）。
    nodes: [],
    latency: {},
    subscriptions: [],
    settings: { selected_node: null },
    ...extra,
  } as never;
}

function renderIn(node: ReactElement) {
  return render(<StoreProvider>{node}</StoreProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snapWith({}));
  mocks.tailLogs.mockResolvedValue([]);
  mocks.clearLogs.mockResolvedValue(undefined);
  mocks.deleteNode.mockResolvedValue(snapWith({}));
  mocks.removeSubscription.mockResolvedValue(snapWith({}));
});

describe("InlineConfirm：机制本身（task-23 B）", () => {
  it("未确认时点原按钮**不执行**，只把确认问句显示出来", () => {
    const onConfirm = vi.fn();
    render(
      <InlineConfirm
        label="删除"
        question="删除「X」？无法撤销。"
        confirmLabel="确认删除"
        onConfirm={onConfirm}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.getByText("删除「X」？无法撤销。")).toBeTruthy();
  });

  it("点「确认删除」才执行，且只执行一次", () => {
    const onConfirm = vi.fn();
    render(
      <InlineConfirm label="删除" question="删除「X」？" confirmLabel="确认删除" onConfirm={onConfirm} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
    // 执行后收起确认态，回到原按钮
    expect(screen.getByRole("button", { name: "删除" })).toBeTruthy();
  });

  it("「取消」不执行", () => {
    const onConfirm = vi.fn();
    render(
      <InlineConfirm label="删除" question="删除「X」？" confirmLabel="确认删除" onConfirm={onConfirm} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "确认删除" })).toBeNull();
  });

  it("Esc 也能退出确认态（临时状态必须有明确退路）", () => {
    const onConfirm = vi.fn();
    render(
      <InlineConfirm label="删除" question="删除「X」？" confirmLabel="确认删除" onConfirm={onConfirm} />,
    );
    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByRole("button", { name: "确认删除" })).toBeNull();
    expect(onConfirm).not.toHaveBeenCalled();
  });
});

describe("三处真实站点都先确认（task-23 B）", () => {
  it("日志「清空」：确认前不调用后端；确认后才调；问句写明会删文件", async () => {
    renderIn(<Logs />);
    await screen.findByText(/核心还没启动过|还没有产生日志/);

    fireEvent.click(screen.getByRole("button", { name: "清空" }));
    expect(mocks.clearLogs).not.toHaveBeenCalled();
    expect(screen.getByText(/会删除日志文件本身，无法撤销/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "确认清空" }));
    await waitFor(() => expect(mocks.clearLogs).toHaveBeenCalledTimes(1));
  });

  it("节点「删除」：确认前不调用后端；问句点名是哪个节点且说明会落盘", async () => {
    mocks.snapshot.mockResolvedValue(
      snapWith({
        nodes: [
          {
            id: "n1",
            name: "香港 · REALITY 01",
            address: "1.2.3.4",
            port: 443,
            protocol: "vless",
            transport: "tcp",
            tls: { server_name: "example.com" },
            mux: null,
            source: { kind: "manual" },
            tags: [],
            raw_uri: null,
          },
        ],
        latency: {},
        settings: { selected_node: null },
      }),
    );
    renderIn(<Nodes />);
    await screen.findByText("香港 · REALITY 01");

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(mocks.deleteNode).not.toHaveBeenCalled();
    expect(screen.getByText(/删除节点「香港 · REALITY 01」/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(mocks.deleteNode).toHaveBeenCalledWith("n1"));
  });

  it("订阅「删除」：问句必须写出会连带删掉几个节点", async () => {
    mocks.snapshot.mockResolvedValue(
      snapWith({
        subscriptions: [
          {
            id: "s1",
            name: "机场 · 主订阅",
            url: "https://example.com/sub",
            enabled: true,
            update_interval_hours: 24,
            last_updated: null,
            last_error: null,
            node_count: 3,
            usage: null,
          },
        ],
      }),
    );
    renderIn(<Subscriptions />);
    await screen.findByText("机场 · 主订阅");

    fireEvent.click(screen.getByRole("button", { name: "删除" }));
    expect(mocks.removeSubscription).not.toHaveBeenCalled();
    expect(screen.getByText(/会同时删除它带来的 3 个节点/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "确认删除" }));
    await waitFor(() => expect(mocks.removeSubscription).toHaveBeenCalledWith("s1"));
  });
});
