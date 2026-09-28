/**
 * 0.9 B3：换节点必须有**进行中提示**，且文案里带**目标节点名**。
 *
 * # 修之前的状态（UX C3 · 实测）
 *
 * 点第二行后命令 `select_node` 已发出，但页面上 `anyWarning: []`、`spinner: 0`；
 * 代码依据 `Nodes.tsx:152` `onSelect={() => void run("select", …)}`。
 * 而换节点会拆掉旧隧道再建（几秒）⇒ 用户以为「换个节点就断网」或以为没点生效而重复点。
 *
 * # 这一组钉住什么
 *
 * 1. 点某一行 ⇒ 出现 `role="status"` 的可见提示，且**含目标节点名**、说清「隧道会重建」；
 * 2. 切换完成 ⇒ 提示收掉（不留一条永远挂着的「正在切换」）；
 * 3. **不加二次确认**（换节点是高频动作，计划 §3 明确不做）；
 * 4. F3：导出模态打开时 `Esc` 关闭（监听挂在 `window` 上，不依赖模态里有焦点）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  selectNode: vi.fn(),
  testLatency: vi.fn(),
  exportNode: vi.fn(),
  deleteNode: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    selectNode: mocks.selectNode,
    testLatency: mocks.testLatency,
    exportNode: mocks.exportNode,
    deleteNode: mocks.deleteNode,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Nodes from "./pages/Nodes";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

/** 预览快照里的第二台（不是当前选中的那台）。 */
const TARGET = "日本 · 大阪 BGP";

async function renderNodes() {
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Nodes />
    </StoreProvider>,
  );
  await screen.findByRole("radiogroup", { name: "节点" });
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.testLatency.mockResolvedValue(scenarioSnapshot());
  mocks.deleteNode.mockResolvedValue(scenarioSnapshot());
});

describe("0.9 B3 · 换节点要有进行中提示（含目标节点名）", () => {
  it("点第二行 ⇒ role=status 的提示里是**目标节点名**，并说清隧道会重建", async () => {
    await renderNodes();
    expect(screen.queryByRole("status"), "没换之前不该有进行中提示").toBeNull();

    // 未决 Promise：把「进行中的那几秒」造出来
    let finish!: (v: unknown) => void;
    mocks.selectNode.mockReturnValue(new Promise((r) => (finish = r)));

    fireEvent.click(screen.getByRole("radio", { name: new RegExp(TARGET) }));

    const notice = await screen.findByRole("status");
    const text = notice.textContent ?? "";
    expect(text, "必须点名**目标**节点（只说「正在切换」等于没说）").toContain(TARGET);
    expect(text, "必须说清会发生什么").toContain("隧道会重建");
    expect(mocks.selectNode, "提示必须对应一条真的发出去的命令").toHaveBeenCalledTimes(1);

    // 切换完成 ⇒ 提示必须收掉
    finish(scenarioSnapshot());
    await waitFor(() => expect(screen.queryByRole("status")).toBeNull());
  });

  it("反例：只是渲染（没换节点）⇒ 不出现进行中提示（防狼来了）", async () => {
    await renderNodes();
    expect(screen.queryByRole("status")).toBeNull();
    expect(mocks.selectNode).not.toHaveBeenCalled();
  });

  it("换节点**不加二次确认**：点一下就走命令，页面上不出现「确认」按钮", async () => {
    await renderNodes();
    mocks.selectNode.mockResolvedValue(scenarioSnapshot());

    fireEvent.click(screen.getByRole("radio", { name: new RegExp(TARGET) }));

    await waitFor(() => expect(mocks.selectNode).toHaveBeenCalledTimes(1));
    expect(screen.queryByRole("button", { name: /^确认/ }), "高频动作不该多一步确认").toBeNull();
  });

  it("F3：导出模态打开时 Esc 关闭（监听挂在 window 上，焦点不在模态里也收得到）", async () => {
    mocks.exportNode.mockResolvedValue({
      node_id: "n-hk-1",
      node_name: "香港 · REALITY 01",
      uri: "vless://example",
      svg: "<svg></svg>",
      lost: [],
    });
    await renderNodes();

    fireEvent.click(screen.getAllByRole("button", { name: "二维码" })[0]!);
    expect(await screen.findByText(/导出「香港 · REALITY 01」/)).toBeTruthy();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText(/导出「香港 · REALITY 01」/)).toBeNull());
  });
});
