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
 * | `Logs.tsx` 诊断说明 | 「已抹掉节点地址…可以直接贴到公开 issue」 | 只说真抹了什么（task-124：地址 → `<addr>`，公开域名/本机地址保留，覆盖不到的形态点名写出） |
 * | `Logs.tsx` 诊断块「复制」 | 裸 `writeText`（失败静默） | 复用 `CopyButton` ⇒ 失败 `role=alert` + 可手动选中的兜底 |
 * | `Logs.tsx` 空态 | `!running` ⇒ 「核心还没启动过」 | 看 `runtime.started_at_unix` |
 * | `Logs.tsx` 等级计数 | 「错误：1 条」（读成总数） | 说明是**已加载**窗口里的条数 |
 */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
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
  globeData: vi.fn(),
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
      globeData: mocks.globeData,
    },
    subscribe: () => () => {},
  };
});

import { TopBar } from "./App";
import Dashboard from "./pages/Dashboard";
import Globe from "./pages/Globe";
import Nodes from "./pages/Nodes";
import Settings from "./pages/Settings";
import { DestChecker } from "./topology/DestChecker";
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
  // `run("start"/"stop", …)` 走的是 `store.tsx:157` `setSnapshot(await action())`
  // —— 返回值**就是**新快照，所以这里必须给完整快照，不能给 `undefined`
  // （`{}`/部分对象更糟：会让 `snapshot.settings` 变成 undefined，把异常挂到
  // 整个 run 的 unhandled error 上，冻结门禁就是这么红的 —— Lead 在 `d95b4ef` 上实测）。
  mocks.start.mockResolvedValue(snap());
  mocks.stop.mockResolvedValue(snap());
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

  it("诊断说明必须与脱敏实现同口径：节点地址会抹、公开域名与本机地址保留、并点名写清覆盖不到的形态", async () => {
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "info", message: "既有日志" },
    ]);
    fireEvent.click(await screen.findByRole("button", { name: "诊断" }));
    const note = await screen.findByText(/已抹掉/);
    const text = note.textContent ?? "";
    expect(text, "必须说清地址被替换成 <addr>").toContain("<addr>");
    expect(text, "还要抹订阅 URL 凭据与 UUID 形状的 token").toContain("UUID");
    // task-124：脱敏做够之后，「节点地址原样保留 + 自己核对」这句已经是假的。
    expect(text, "不许再说节点地址会原样保留").not.toContain("节点地址、域名与 IP:port 会原样保留");
    expect(text, "不许再把「自己核对」当兜底").not.toContain("请自己核对");
    expect(text, "必须说清公开目标域名与本机地址保留").toContain("www.baidu.com");
    // 裁决 3：订阅 URL **只抹凭据、保留主机名**，必须点名写清（别让人以为整条都没了）；
    // 句子会跨 JSX 行 ⇒ 抹掉空白再比。
    const flat = text.replace(/\s+/g, "");
    expect(flat, "订阅 URL 必须点名说清「只抹凭据、主机名保留」").toContain(
      "只抹凭据、主机名（机场域名）会保留",
    );
    expect(flat, "用户主目录折成 /Users/<user>/… 也要写出来").toContain("/Users/<user>/…");
    expect(
      text,
      "覆盖不到的形态要**逐个点名**（base64 与非 HOME 的用户名路径），不能笼统地让用户自查",
    ).toContain("base64");
    expect(flat, "第二种覆盖不到的形态也要点名（/var/folders/…）").toContain("/var/folders/…");
    expect(text, "不许再声称「只有一种」覆盖不到").not.toContain("唯一覆盖不到");
    expect(text, "不能再说「可以直接贴到公开的 issue 里」").not.toContain("可以直接贴到公开的 issue");
  });

  it("诊断块的「复制」失败必须可见 —— 原来是裸 writeText，剪贴板被拒时毫无提示", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn(() => Promise.reject(new Error("NotAllowedError"))) },
      configurable: true,
    });
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "info", message: "既有日志" },
    ]);
    fireEvent.click(await screen.findByRole("button", { name: "诊断" }));
    await screen.findByText(/已抹掉/);
    // 工具栏也有一个「复制」（复制日志），所以只在诊断块里找。
    const head = document.querySelector(".logs-diag__head") as HTMLElement;
    fireEvent.click(within(head).getByRole("button", { name: "复制" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent, "失败必须说出来，不许静默").toContain("复制失败");
    expect(alert.textContent).toContain("剪贴板不可用");
    const box = screen.getByLabelText("手动复制内容") as HTMLTextAreaElement;
    expect(box.value, "兜底里要放报告正文本身").toBe("（诊断报告正文）");
    expect(box.readOnly).toBe(true);
  });

  it("反例：剪贴板成功 ⇒ 给「已复制」确认，且不出现失败块", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn(() => Promise.resolve()) },
      configurable: true,
    });
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "info", message: "既有日志" },
    ]);
    fireEvent.click(await screen.findByRole("button", { name: "诊断" }));
    await screen.findByText(/已抹掉/);
    const head = document.querySelector(".logs-diag__head") as HTMLElement;
    fireEvent.click(within(head).getByRole("button", { name: "复制" }));
    expect(await screen.findByText(/已复制到剪贴板/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("等级计数必须写明是「已加载的窗口」里的条数，而不是总数", async () => {
    await renderWith(snap(), <Logs />, [
      { seq: 1, ts_unix: 1, source: "core", level: "error", message: "一条错误" },
    ]);
    const btn = await screen.findByRole("button", { name: /错误/ });
    expect(btn.getAttribute("title")).toBe("错误：已加载的 1 条中有 1 条");
  });
});

describe("task-120 · 节点页不能说「正在使用」（那是选中，不是数据面）", () => {
  it("核心没在跑时也不能说「正在使用这个节点」/「当前」", async () => {
    await renderWith(snap({ running: false }), <Nodes />);
    const row = (await screen.findByText("香港 · REALITY 01")).closest(".node-row") as HTMLElement;
    expect(row.getAttribute("title")).toContain("已选中");
    expect(row.textContent).toContain("已选中");
    expect(row.textContent).not.toContain("当前");
    expect(row.getAttribute("title")).not.toContain("正在使用");
  });

  it("反例：核心在跑时也**不**改口说「正在使用」—— 那是 Dashboard 的活（connected && selected）", async () => {
    await renderWith(snap({ running: true }), <Nodes />);
    const row = (await screen.findByText("香港 · REALITY 01")).closest(".node-row") as HTMLElement;
    expect(row.getAttribute("title")).not.toContain("正在使用");
  });
});

describe("task-120 · 地球仪「出口累计流量（实测）」必须看 traffic_ok", () => {
  const geo = () => ({
    city: "X",
    country: "Y",
    ip: "203.0.113.1",
    lat: 1,
    lon: 2,
    isp: null,
    source: "ip-api",
    sources: [],
    consistent: true,
  });

  // task-181：`task-179` 把 `self_check` / `route.traffic` 变成**真类型的必填字段**
  // ⇒ 夹具必须同形（否则读到 `undefined.verified` 会抛，而不是「显示旧文案」）。
  const globeWith = (route: Record<string, unknown>) => ({
    route: {
      from: geo(),
      to: geo(),
      node_name: "XrayTun-US",
      traffic: { tag: "node-n1", is_node_outbound: true, verified: true, reason: null },
      ...route,
    },
    origin: geo(),
    error: null,
    self_check: { ip: "203.0.113.1", bound_interface: "en0", trusted: true, reason: null },
  });

  // task-181 更新口径：判据从 `traffic_ok` 换成 `traffic.verified`（task-179 的字段是它的超集：
  // 「查统计失败」也在 `verified === false` 里，并带具体 `reason`）。断言强度不变 ——
  // 仍然是「**不许**显示数字、必须说出来」，只是文案换成新口径那句。
  it("归属未验证 ⇒ 不得显示「0 B（实测）」，必须说清并不显示数字", async () => {
    mocks.snapshot.mockResolvedValue(snap());
    mocks.tailLogs.mockResolvedValue([]);
    mocks.globeData.mockResolvedValue(
      globeWith({
        bytes: 0,
        traffic_ok: false,
        counter_resets: 0,
        traffic: { tag: null, is_node_outbound: false, verified: false, reason: "查统计失败（核心没在跑）⇒ 归属未验证" },
      }),
    );
    render(
      <StoreProvider>
        <Globe />
      </StoreProvider>,
    );
    await screen.findByText(/出口流量归属未验证/);
    expect(screen.queryByText(/出口累计流量（实测）/)).toBeNull();
    expect(screen.queryByText(/节点出站累计/)).toBeNull();
  });

  it("反例：已验证到节点 ⇒ 显示数字 + 续接说明", async () => {
    mocks.snapshot.mockResolvedValue(snap());
    mocks.tailLogs.mockResolvedValue([]);
    mocks.globeData.mockResolvedValue(
      globeWith({ bytes: 1024 * 1024, traffic_ok: true, counter_resets: 2 }),
    );
    render(
      <StoreProvider>
        <Globe />
      </StoreProvider>,
    );
    // task-181 新口径：已验证到节点时才说「节点出站累计（出站 <tag>）」
    await screen.findByText(/节点出站累计（出站 node-n1）/);
    expect(screen.getByText(/核心重启过 2 次/)).toBeTruthy();
  });
});

describe("task-120 · 判定器必须说清结论的适用范围", () => {
  it("geoAvailable ⇒ 必须写明是「按 443/tcp 求值」，不能再只说「确定的」", async () => {
    render(<DestChecker geoAvailable />);
    await screen.findByText(/一条结论|结论/);
    const desc = document.querySelector(".page__desc")?.textContent ?? "";
    expect(desc).toContain("443/tcp");
    expect(desc).toContain("端口");
  });

  it("反例：没有 geo 数据时不得给出「确定的」结论", async () => {
    render(<DestChecker geoAvailable={false} />);
    const desc = document.querySelector(".page__desc")?.textContent ?? "";
    expect(desc).not.toContain("确定性");
    expect(desc).not.toContain("对拍过");
    expect(desc).toContain("无法判定");
  });
});

describe("task-120 · 节点「未测」不能把「测不到」并进去", () => {
  const withProbe = (probe: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return {
      ...base,
      latency: { ...base.latency, [SELECTED]: probe },
    } as never;
  };

  // 「未测」这三个字**两个徽章都会用**：距离徽章（本次修复的对象）与可用性徽章
  // （`available === null` 时它也写「未测」，那是正确的）。所以按**数量**断言：
  // 距离徽章不再贡献那个「未测」。
  it("探测过 + RTT 采样失败（error=null）⇒ 距离徽章写「距离未知」，不再写「未测」", async () => {
    await renderWith(
      withProbe({ node_id: SELECTED, node_name: "x", server_rtt_ms: null, available: true, error: null }),
      <Nodes />,
    );
    await screen.findByText("香港 · REALITY 01");
    expect(screen.getByText("距离未知")).toBeTruthy();
    // available=true ⇒ 可用性徽章写「可用」，于是「未测」一个都不该剩
    expect(screen.queryAllByText("未测").length).toBe(0);
  });

  it("反例：完全没探测过（latency 里没有这一项）⇒ 距离与可用性徽章都是「未测」", async () => {
    const base = scenarioSnapshot();
    const latency: Record<string, unknown> = { ...base.latency };
    delete latency[SELECTED];
    await renderWith({ ...base, latency } as never, <Nodes />);
    await screen.findByText("香港 · REALITY 01");
    expect(screen.queryAllByText("未测").length).toBe(2);
    expect(screen.queryByText("距离未知")).toBeNull();
  });

  it("反例：探测过且明确失败（有 error）⇒ 「测不到」，不是「未测」", async () => {
    await renderWith(
      withProbe({ node_id: SELECTED, node_name: "x", server_rtt_ms: null, available: false, error: "探针超时" }),
      <Nodes />,
    );
    await screen.findByText("香港 · REALITY 01");
    expect(screen.getByText("测不到")).toBeTruthy();
    expect(screen.queryAllByText("未测").length).toBe(0);
  });
});

describe("task-120 · 设置页 DNS 探测结果", () => {
  function settingsSnap(over: {
    autoSelect?: boolean;
    chosen?: string | null;
    direct?: string[];
    probes?: unknown[];
    logLevel?: string;
  } = {}) {
    const base = scenarioSnapshot();
    return {
      ...base,
      settings: {
        ...base.settings,
        log_level: over.logLevel ?? base.settings.log_level,
        dns: {
          ...base.settings.dns,
          auto_select: over.autoSelect ?? base.settings.dns.auto_select,
          direct_servers: over.direct ?? base.settings.dns.direct_servers,
        },
      },
      dns: {
        ...base.dns,
        chosen: over.chosen === undefined ? base.dns.chosen : over.chosen,
        probes: over.probes ?? base.dns.probes,
      },
    } as never;
  }

  const probe = (over: Record<string, unknown>) => ({
    server: "223.5.5.5",
    label: "阿里 DNS",
    kind: "domestic",
    transport: "plain_udp",
    latency_ms: null,
    answered: true,
    suspect: false,
    note: null,
    ...over,
  });

  it("answered=true + latency_ms=null ⇒ 不能写「不通」", async () => {
    await renderWith(settingsSnap({ probes: [probe({})] }), <Settings focusSection="set-dns-probe" />);
    // 国内/国外两组各渲染一份，所以用 findAll
    expect((await screen.findAllByText("答得出，量不到延迟")).length).toBeGreaterThan(0);
    expect(screen.queryAllByText("不通").length).toBe(0);
  });

  it("反例：answered=false ⇒ 这才是「不通」", async () => {
    await renderWith(
      settingsSnap({ probes: [probe({ answered: false })] }),
      <Settings focusSection="set-dns-probe" />,
    );
    expect((await screen.findAllByText("不通")).length).toBeGreaterThan(0);
    expect(screen.queryAllByText("答得出，量不到延迟").length).toBe(0);
  });

  it("反例：note 非空 ⇒ 「未探测」，不判成不通", async () => {
    await renderWith(
      settingsSnap({ probes: [probe({ answered: false, note: "节点未连接，未探测" })] }),
      <Settings focusSection="set-dns-probe" />,
    );
    expect((await screen.findAllByText("未探测")).length).toBeGreaterThan(0);
    expect(screen.queryAllByText("不通").length).toBe(0);
  });

  it("自动选优关闭 ⇒ 不能说「当前首选」，必须写出配置里真正生效的那个", async () => {
    await renderWith(
      settingsSnap({ autoSelect: false, chosen: "223.5.5.5", direct: ["119.29.29.29"] }),
      <Settings focusSection="set-dns-probe" />,
    );
    expect((await screen.findAllByText(/自动选优已关闭/)).length).toBeGreaterThan(0);
    expect(screen.queryAllByText("119.29.29.29").length).toBeGreaterThan(0);
    expect(screen.queryAllByText(/当前首选/).length).toBe(0);
  });

  it("反例：自动选优开启 ⇒ 才说「本次检测首选（会写回配置）」", async () => {
    await renderWith(
      settingsSnap({ autoSelect: true, chosen: "223.5.5.5", direct: ["119.29.29.29"] }),
      <Settings focusSection="set-dns-probe" />,
    );
    expect((await screen.findAllByText(/自动选优已开启/)).length).toBeGreaterThan(0);
    expect(screen.queryAllByText(/当前首选/).length).toBe(0);
  });

  const optionValues = () =>
    Array.from(document.querySelectorAll("option")).map((o) => o.getAttribute("value"));

  it("日志级别下拉必须是 Xray 真认得的取值（有 none，没有 silent）", async () => {
    await renderWith(settingsSnap({ logLevel: "warning" }), <Settings focusSection="set-entry" />);
    await screen.findByText("日志级别");
    expect(optionValues()).toContain("none");
    expect(optionValues()).not.toContain("silent");
  });

  it("反例：配置里存着历史值 silent ⇒ 如实标出来，不让下拉显示成别的值", async () => {
    await renderWith(settingsSnap({ logLevel: "silent" }), <Settings focusSection="set-entry" />);
    await screen.findByText("日志级别");
    const legacy = Array.from(document.querySelectorAll("option")).find(
      (o) => o.getAttribute("value") === "silent",
    );
    expect(legacy, "历史值必须作为一项出现，否则下拉会显示成别的值").toBeTruthy();
    expect(legacy!.textContent).toContain("Xray 不识别");
  });
});

describe("task-120 ·「保存并重启核心」必须看保存结果", () => {
  /** 让草稿变脏：改一个配置字段（内核路径输入框）。 */
  async function makeDirtyAndRestart() {
    const field = (await screen.findByText("Xray 可执行文件路径")).closest(".field") as HTMLElement;
    const input = field.querySelector("input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "/tmp/xray" } });
    fireEvent.click(screen.getByRole("button", { name: "保存并重启核心" }));
  }

  it("保存失败（后端拒绝）⇒ **不得**继续 stop/start，也不得把错误抹掉", async () => {
    mocks.saveSettings.mockRejectedValue(new Error("端口 80 需要管理员权限"));
    await renderWith(snap(), <Settings focusSection="set-core" />);
    await makeDirtyAndRestart();
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
    // 关键：核心没被碰过 —— 用旧设置重启会让人以为改动已经生效
    expect(mocks.stop).not.toHaveBeenCalled();
    expect(mocks.start).not.toHaveBeenCalled();
    // 注：后端原文由 App 顶部的错误横幅渲染（Settings 自己不画那个横幅），
    // 所以这里不重复断言它是否存在，只钉住「失败就不重启」这条不变量。
  });

  it("反例：保存成功 ⇒ 才真的重启核心", async () => {
    mocks.saveSettings.mockResolvedValue(snap());
    mocks.stop.mockResolvedValue(snap());
    mocks.start.mockResolvedValue(snap());
    await renderWith(snap(), <Settings focusSection="set-core" />);
    await makeDirtyAndRestart();
    await waitFor(() => expect(mocks.stop).toHaveBeenCalledTimes(1));
    expect(mocks.start).toHaveBeenCalledTimes(1);
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
