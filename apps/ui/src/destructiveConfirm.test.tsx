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
 * 2. **六处**真实站点**都**先确认再调用后端 —— 这是防「只在某一处接上、另几处忘了」的回归：
 *    日志清空、节点删除、订阅删除（task-23 B），以及设置页的
 *    修复网络 / 卸载 helper / 回退到随包版本（task-65）。
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
  // task-65：设置页三处破坏性操作
  restoreStale: vi.fn(),
  uninstallHelper: vi.fn(),
  revertManagedUpdate: vi.fn(),
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
    restoreStale: mocks.restoreStale,
    uninstallHelper: mocks.uninstallHelper,
    revertManagedUpdate: mocks.revertManagedUpdate,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import { InlineConfirm } from "./InlineConfirm";
import Logs from "./pages/Logs";
import Nodes from "./pages/Nodes";
import Settings from "./pages/Settings";
import Subscriptions from "./pages/Subscriptions";
import { scenarioSnapshot } from "./previewSnapshot";
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

/**
 * 设置页要完整快照（它读 `helper` / `update` / `settings` 等多块）。
 *
 * 用真实预览快照（`scenarioSnapshot()`）而不是手搓最小骨架 —— 尤其 `update` 的
 * 字段之间有相互关系（`core_managed` 决定「回退到随包版本」那颗按钮是否存在）。
 *
 * `focusSection` 用 task-48 的两级结构直接落到分类，不必先点标签。
 */
function settingsSnap(over: Record<string, unknown> = {}) {
  return { ...scenarioSnapshot(), ...over } as never;
}

function renderSettings(focusSection: string, snap: unknown = settingsSnap()) {
  mocks.snapshot.mockResolvedValue(snap);
  return render(
    <StoreProvider>
      <Settings focusSection={focusSection} />
    </StoreProvider>,
  );
}

/** 当前确认问句的**正文** —— 断言必须落在它身上，而不是「页面上随便哪儿出现了这几个字」。 */
function questionText(): string {
  const el = document.querySelector(".confirm__question");
  if (!el) throw new Error("没有出现确认问句");
  return el.textContent ?? "";
}

/**
 * 点按钮**每次都重新查**。
 *
 * ⚠️ 不能把 `getByRole(...)` 的结果存起来复用：`InlineConfirm` 在「未确认 ↔ 确认态」
 * 之间换的是**另一棵子树**，旧节点会被卸载 —— 对着已卸载的节点 `fireEvent.click`
 * 什么都不会发生，测试会以「找不到确认按钮」这种**误导性**的方式失败。
 */
function clickButton(name: string | RegExp): void {
  fireEvent.click(screen.getByRole("button", { name }));
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snapWith({}));
  mocks.tailLogs.mockResolvedValue([]);
  mocks.clearLogs.mockResolvedValue(undefined);
  mocks.deleteNode.mockResolvedValue(snapWith({}));
  mocks.removeSubscription.mockResolvedValue(snapWith({}));
  mocks.restoreStale.mockResolvedValue(settingsSnap());
  mocks.uninstallHelper.mockResolvedValue(settingsSnap());
  mocks.revertManagedUpdate.mockResolvedValue(settingsSnap());
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

describe("六处真实站点都先确认（task-23 B + task-65）", () => {
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
        // task-120：这个数字**必须来自 nodes[].source.id**（后端就是按它真删的），
        // 不能再用 `sub.node_count` —— 那是「上次刷新解析出几条」的快照值，
        // 手动删节点/导入去重都不会回写。所以这里把 3 个节点真的放进列表里，
        // 让断言钉住「问句里的数量 = 即将被删掉的节点数」。
        nodes: [
          { id: "n1", name: "节点 1", source: { kind: "subscription", id: "s1" } },
          { id: "n2", name: "节点 2", source: { kind: "subscription", id: "s1" } },
          { id: "n3", name: "节点 3", source: { kind: "subscription", id: "s1" } },
        ],
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

  // ---- task-65：设置页的三处系统级操作 ------------------------------------

  it("「修复网络」：确认前不调用后端；问句写明**会拆掉正在生效的隧道并回到直连**", async () => {
    renderSettings("set-helper");
    const LABEL = "修复网络（回滚遗留配置）";
    await screen.findByRole("button", { name: LABEL });

    clickButton(LABEL);
    expect(mocks.restoreStale, "点一下就直接改了系统网络配置").not.toHaveBeenCalled();
    // 后果必须是**具体**的：回滚什么、最终网络是什么状态。
    // （读码确认：helper 的 `Request::Restore` 会 tear_down_live_session + force_cleanup，
    //   即不只清遗留会话，**正在生效的隧道也会被拆**。）
    const q = questionText();
    expect(q, "没写回滚什么").toContain("路由与 DNS");
    expect(q, "没写最终会变成什么状态").toContain("直连");
    expect(q, "没写会打断正在进行的连接").toContain("连接会断");

    // 取消 → 不调用，且回到原按钮（此时**必须重新查**，见 clickButton 的注释）
    clickButton("取消");
    expect(mocks.restoreStale).not.toHaveBeenCalled();

    // 确认 → 恰好一次
    clickButton(LABEL);
    clickButton("确认修复网络");
    await waitFor(() => expect(mocks.restoreStale).toHaveBeenCalledTimes(1));
  });

  it("「卸载 helper」：确认前不调用后端；问句必须含 **TUN** 与 **管理员密码**", async () => {
    renderSettings("set-helper");
    await screen.findByRole("button", { name: "卸载 helper" });

    clickButton("卸载 helper");
    expect(mocks.uninstallHelper, "点一下就把 LaunchDaemon 删了").not.toHaveBeenCalled();
    const q = questionText();
    // 这两条是用户真正会在意的代价：功能没了、还要再输一次密码。
    expect(q, "没写 TUN 模式会不可用").toContain("TUN");
    expect(q, "没写重装要再输管理员密码").toContain("管理员密码");
    expect(q, "没写它还会回滚路由与 DNS").toContain("回滚");

    clickButton("确认卸载");
    await waitFor(() => expect(mocks.uninstallHelper).toHaveBeenCalledTimes(1));
  });

  it("「回退到随包版本」：确认前不调用后端；问句写明删什么、并**报出受管版本号**", async () => {
    renderSettings(
      "set-update",
      settingsSnap({
        update: {
          ...scenarioSnapshot().update,
          core_managed: true,
          core_managed_version: "26.9.12",
        },
      }),
    );
    const LABEL = "回退到随包版本";
    await screen.findByRole("button", { name: LABEL });

    clickButton(LABEL);
    expect(mocks.revertManagedUpdate, "点一下就把受管更新删了").not.toHaveBeenCalled();
    const q = questionText();
    expect(q, "没写会删掉受管更新").toContain("受管更新");
    // 能读到的版本号（当前受管版本）必须写出来 —— 用户要知道自己放弃了什么。
    expect(q, "没报出将被删掉的受管版本号").toContain("26.9.12");
    // 目标版本读不到（随包核心版本没暴露在快照里），所以只能如实写「包内自带的那一版」。
    expect(q, "没写核心会变成哪一版").toContain("包内自带");
    expect(q, "没写何时生效").toContain("重新连接");

    clickButton("取消");
    expect(mocks.revertManagedUpdate).not.toHaveBeenCalled();

    clickButton(LABEL);
    clickButton("确认回退");
    await waitFor(() => expect(mocks.revertManagedUpdate).toHaveBeenCalledTimes(1));
  });

  it("反例防线：「回退到随包版本」只在**确实装了受管更新**时出现（core_managed=false 不显示）", async () => {
    renderSettings("set-update"); // 预览快照默认 core_managed: false
    await screen.findByRole("button", { name: "检查更新" });
    expect(screen.queryByRole("button", { name: "回退到随包版本" })).toBeNull();
  });

  // ---- task-65 补充：界面不许承诺做不到的事 --------------------------------

  it("没有实现的「退出时还原系统代理设置」**不得留在界面上**（勾了什么都不会发生）", async () => {
    renderSettings("set-misc");
    await screen.findByRole("button", { name: "打开数据目录" });

    // 同一个「其他」区里**该留的要留**：这条是真实生效的速度开关，
    // 用它证明我摘掉的是对的那一个，而不是整块删了。
    expect(screen.getByText(/在标题栏与菜单栏显示实时网速/)).toBeTruthy();
    // 速率说明是**真的**（来自核心流量计数器），保留。
    expect(screen.getByText(/速率来自核心的流量计数器/)).toBeTruthy();

    // 而这一条：`settings.restore_system_proxy_on_exit` 全仓无逻辑引用，
    // 且系统代理模式从未改过系统代理设置（docs/07 已知未实现项第 6 条）。
    expect(
      screen.queryByText("退出时还原系统代理设置"),
      "这个开关没有对象：勾与不勾都不会改变任何行为，界面不该留着它",
    ).toBeNull();
  });
});
