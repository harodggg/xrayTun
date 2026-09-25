/**
 * task-3「界面健壮性与诚实性」：**不许撒谎，也不许吓人**。
 *
 * # 这一文件钉住什么（每条都先写「原来用户会看到什么错的」）
 *
 * | # | 原来用户会看到 | 事实 | 本文件的断言 |
 * |---|---|---|---|
 * | 1 | `[object Object]` / `{}` 这种「既不是原因也不是办法」的乱码 | `invoke` 的 reject 载荷不保证是字符串 | `humanError` 只读真实文案，读不出来就明说读不出来 |
 * | 2 | 一个错误码，没有任何出路 | 失败必须给动作 | `nextSteps`/`failureActions`：助手⇒重装助手、门禁/节点⇒换节点、兜底⇒看日志 |
 * | 3 | `appStatus` 在快照没回来时红着说「未找到核心」 | 那时是**不知道** | `snapshotLoaded:false` ⇒ 中性「正在读取状态…」 |
 * | 4 | `intentStatus()` 读失败时页面照写「拦截 0 条 / 引擎未运行 / 缓存 0 条」 | 一次 IPC 故障被说成三项确定事实 | Intent：显示「读不到（见上面的错误）」且**不含**「拦截 0 条」 |
 * | 5 | `mitmStatus()` 读失败被静默吞掉，页面写「代理没在跑 / 证书还没生成过」 | 读不到 ≠ 没在跑 | Intent：三道闸门都显示「读不到…」，且不含「没在跑」「还没生成过」 |
 * | 6 | MITM「生效」在核心从没启动时写「引导规则已随核心生效」 | `core_steering===null` ⇒ 一条引导规则都不在任何核心里 | U1：单独一态 |
 * | 7 | 审计列「生效 = 是」被读成「正在被拦」 | `applied` 只表示「会构成规则」 | U2：列名「会生成规则」，演练模式显示「不会（演练模式）」 |
 * | 8 | 「生效的规则 拦截 N 条」与同屏「还没下发给核心」互相否定 | 那是**当前规则集合** | U5：改名 + 待下发时写明 |
 * | 9 | 连接失败横幅带 `**` 与坍缩的换行、一个按钮都没有 | 后端文案是给用户看的；动作要能点 | App：去掉 `**`、保留换行、给出可点动作并能真的导航 |
 * | 10 | 文案叫用户点「断开」，而那一刻按钮写「连接」 | 门禁失败发生在接管路由之前 | U8：同屏说清按钮现在叫什么 |
 * | 11 | 启动即崩后 `logs/panic.log` 在 App 内 0 命中 | panic hook 确实写了它 | U4：日志页常驻一行 + 「打开数据目录」动作 |
 */
import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  openDataDir: vi.fn(),
  incidentAnomalyCount: vi.fn(),
  saveSettings: vi.fn(),
  intentStatus: vi.fn(),
  intentAudit: vi.fn(),
  intentExplain: vi.fn(),
  intentAllow: vi.fn(),
  intentApply: vi.fn(),
  intentClearCache: vi.fn(),
  mitmStatus: vi.fn(),
  mitmInstallCa: vi.fn(),
  mitmRemoveCa: vi.fn(),
  mitmApply: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      start: mocks.start,
      stop: mocks.stop,
      openDataDir: mocks.openDataDir,
      incidentAnomalyCount: mocks.incidentAnomalyCount,
      saveSettings: mocks.saveSettings,
      intentStatus: mocks.intentStatus,
      intentAudit: mocks.intentAudit,
      intentExplain: mocks.intentExplain,
      intentAllow: mocks.intentAllow,
      intentApply: mocks.intentApply,
      intentClearCache: mocks.intentClearCache,
      mitmStatus: mocks.mitmStatus,
      mitmInstallCa: mocks.mitmInstallCa,
      mitmRemoveCa: mocks.mitmRemoveCa,
      mitmApply: mocks.mitmApply,
    },
    subscribe: () => () => {},
  };
});

import App from "./App";
import {
  buttonNameNote,
  failureActions,
  failureAdvice,
  humanError,
  nextSteps,
  plainOneLine,
  stripMarkup,
} from "./failure";
import Logs, { PANIC_LOG_PATH } from "./pages/Logs";
import Intent, { mitmGates, mitmGateSummary } from "./pages/Intent";
import { collectNotices } from "./pages/Dashboard";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import { appStatus } from "./topbarStatus";
import type { IntentAuditRecord, IntentSummary, MitmStatus } from "./types";

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

function mitmStatus(over: Partial<MitmStatus> = {}): MitmStatus {
  return {
    enabled: true,
    active: true,
    running: true,
    listen_port: 10810,
    upstream_port: 10811,
    domains: ["promoted.example"],
    block_quic: false,
    ca_fingerprint: "AA:BB",
    ca_expires_at: "2028-09-25",
    stats: null,
    note: null,
    applied: null,
    core_steering: true,
    core_restart_required: false,
    ...over,
  };
}

function summary(over: Partial<IntentSummary> = {}): IntentSummary {
  return {
    active: true,
    enabled: true,
    drill: true,
    model: "jev-1.13-free",
    gateway: "https(mock)",
    fingerprint: "abc123",
    pending: 0,
    cache_len: 0,
    built_at_unix: 1,
    block_rules: 0,
    allow_rules: 0,
    skipped_rules: 0,
    rules_pending_apply: false,
    applied_at_unix: null,
    gateway_calls: 0,
    gateway_errors: 0,
    cache_hits: 0,
    blocked: 0,
    note: null,
    ...over,
  };
}

function auditRow(over: Partial<IntentAuditRecord> = {}): IntentAuditRecord {
  return {
    ts_unix: 1_700_000_000,
    host: "ads.example",
    outcome: "block",
    reason: null,
    category: "ad_or_monetization",
    ads_intent: 0.97,
    risk_of_breakage: 0.05,
    choice_confidence: 0.93,
    effective_min: 0.85,
    applied: false,
    cache_hit: false,
    model: "jev-latest",
    usage: null,
    context_sent: null,
    ...over,
  };
}

function snapshotWith(patch: {
  intent?: Record<string, unknown>;
  mitm?: Record<string, unknown>;
  runtime?: Record<string, unknown>;
}) {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: {
      ...base.settings,
      intent: { ...base.settings.intent, ...(patch.intent ?? {}) },
      mitm: { ...base.settings.mitm, ...(patch.mitm ?? {}) },
    },
    runtime: { ...base.runtime, ...(patch.runtime ?? {}) },
  };
}

/**
 * 挂载意图过滤页并等它稳定。
 *
 * `ready` 是一个谓词而不是 `findByText` 锚点：有几条要断言的文案会**同时出现在
 * 多行里**（例如「读不到（见上面的错误）」在引擎/规则/缓存/网关四行都有），
 * `findByText` 会因为匹配到多个元素而失败；用谓词既精确又只表达「等它到这一步」。
 */
async function mountIntent(snap: unknown = snapshotWith({}), ready?: () => boolean) {
  mocks.snapshot.mockResolvedValue(snap);
  render(
    <StoreProvider>
      <Intent />
    </StoreProvider>,
  );
  await screen.findByText("意图过滤");
  if (ready) await vi.waitFor(() => expect(ready()).toBe(true));
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snapshotWith({}));
  mocks.tailLogs.mockResolvedValue([]);
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  mocks.start.mockResolvedValue(scenarioSnapshot());
  mocks.stop.mockResolvedValue(scenarioSnapshot());
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
  mocks.intentStatus.mockResolvedValue(summary());
  mocks.intentAudit.mockResolvedValue([]);
  mocks.mitmStatus.mockResolvedValue(mitmStatus());
});

// ---------------------------------------------------------------------------
// 1. 失败的「人话化」
// ---------------------------------------------------------------------------

describe("humanError：任何 invoke 失败都必须有人话（原来会看到 [object Object] / {}）", () => {
  it("带 message 的对象 ⇒ 用 message，而不是 JSON", () => {
    expect(humanError({ message: "核心没有可执行文件" })).toBe("核心没有可执行文件");
    expect(humanError({ reason: "状态锁不可用" })).toBe("状态锁不可用");
    // 嵌套一层（Tauri 桥接层会包一层 error）
    expect(humanError({ error: { message: "节点握手失败" } })).toBe("节点握手失败");
  });

  it("空对象 ⇒ 明说「后端没给原因」，绝不输出 `{}` / `[object Object]`", () => {
    const t = humanError({});
    expect(t).toContain("没有给出原因");
    expect(t).not.toContain("[object Object]");
    expect(t).not.toContain("{}");
  });

  it("循环引用（原来走 `catch { String(e) }` ⇒ `[object Object]`）⇒ 仍有可读说法", () => {
    const cyclic: Record<string, unknown> = { a: 1 };
    cyclic.self = cyclic;
    const t = humanError(cyclic);
    expect(t).not.toContain("[object Object]");
    expect(t).toContain("没有给出原因");
  });

  it("null / undefined / 空字符串都各有说法（不编原因、也不当成功）", () => {
    expect(humanError(null)).toContain("null");
    expect(humanError(undefined)).toContain("undefined");
    expect(humanError("")).toContain("空字符串");
    expect(humanError(new Error("传输层炸了"))).toBe("传输层炸了");
  });

  it("只给了错误码 ⇒ 说出来，但**不把一个码当成原因**", () => {
    const t = humanError({ code: 503 });
    expect(t).toContain("错误码 503");
    expect(t).toContain("没有说明原因");
  });
});

// ---------------------------------------------------------------------------
// 2. 下一步动作
// ---------------------------------------------------------------------------

describe("下一步动作：不是只给错误码（原来连接失败横幅一个按钮都没有）", () => {
  it("任何失败都至少给一条能做的事（看日志）——线索不足时不编具体归因", () => {
    const steps = nextSteps("完全看不懂的内部故障");
    expect(steps).toHaveLength(1);
    expect(steps[0]).toContain("日志");
  });

  it("助手类 ⇒ 重装助手；节点/连接类 ⇒ 换节点", () => {
    expect(nextSteps("helper 建立 TUN 失败：协议版本不匹配（旧助手）").join("")).toContain(
      "重装助手",
    );
    expect(nextSteps("节点连接超时：经它发出的真实请求拿不到响应").join("")).toContain("换一个节点");
    expect(normalize(nextSteps("端口 10808 被占用"))).toContain("端口");
  });

  it("可点动作与线索一一对应：助手类第一动作是「去重装助手」，节点类第一动作是「去换一个节点」", () => {
    const helper = failureActions("helper 协议版本不匹配（旧助手）");
    expect(helper[0]).toEqual({ id: "reinstall-helper", label: "去重装助手" });
    expect(helper.map((a) => a.id)).toContain("open-logs");

    const gate = failureActions("节点通过了 TCP 检查，但经它发出的真实请求拿不到响应");
    expect(gate[0]).toEqual({ id: "change-node", label: "去换一个节点" });
  });

  it("failureAdvice 同时给出原因与动作（store 横幅用同一对）", () => {
    const a = failureAdvice({ message: "端口 10808 被占用" });
    expect(a.text).toBe("端口 10808 被占用");
    expect(a.steps.length).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// 3. 工程记号与按钮名
// ---------------------------------------------------------------------------

describe("后端文案里的 `**` 与「断开」按钮名（原来用户看到星号、且照着找不到按钮）", () => {
  it("stripMarkup 去掉成对 `**`，保留换行", () => {
    const raw = "真实请求拿不到响应。\n**已在接管默认路由之前中止**，系统网络未被改动。";
    const t = stripMarkup(raw);
    expect(t).not.toContain("**");
    expect(t).toContain("已在接管默认路由之前中止");
    expect(t).toContain("\n");
  });

  it("buttonNameNote：核心没在跑 + 文案提「断开」⇒ 说清按钮现在叫「连接」；在跑时不啰嗦", () => {
    const text = "如果整台 Mac 都上不了网，先点「断开」恢复直连。";
    expect(buttonNameNote(text, false)).toContain("写的是「连接」");
    expect(buttonNameNote(text, true)).toBeNull();
    expect(buttonNameNote("节点超时", false)).toBeNull();
  });

  it("plainOneLine：去 `**`、换行压成空格（tooltip / 读屏 live region 用）", () => {
    const raw = "第一行。\n**第二行**；\n   第三行 有   多空格。";
    const t = plainOneLine(raw);
    expect(t).not.toContain("**");
    expect(t).not.toContain("\n");
    expect(t).toBe("第一行。 第二行； 第三行 有 多空格。");
  });
});

// ---------------------------------------------------------------------------
// 4. 「不知道」不许显示成故障/正常
// ---------------------------------------------------------------------------

describe("appStatus：快照没回来时是「不知道」，不是「未找到核心」", () => {
  const base = {
    mode: "tun" as const,
    running: false,
    routesCommitted: false,
    lastError: null,
    corePath: null,
    recovery: { phase: "idle" as const, text: null, hint: null, button: "connect" as const, justRecovered: false },
    socksPort: null,
    httpPort: null,
  };

  it("snapshotLoaded=false ⇒ 中性「正在读取状态…」，既不是 failed 也不是绿", () => {
    const st = appStatus({ ...base, snapshotLoaded: false });
    expect(st.label).not.toBe("未找到核心");
    expect(st.tone).toBe("off");
    expect(st.detail).toContain("无法判断");
  });

  it("反例：快照回来了且 core.path 为 null ⇒ 才是「未找到核心」", () => {
    const st = appStatus({ ...base, snapshotLoaded: true });
    expect(st.label).toBe("未找到核心");
  });

  it("连接失败的状态里带下一步（原来只有一句错误码）", () => {
    const st = appStatus({ ...base, corePath: "/xray", lastError: "节点连接超时：拿不到响应" });
    expect(st.sub).toContain("节点连接超时");
    expect(st.sub).toContain("下一步");
    expect(st.sub).toContain("换一个节点");
  });

  // ---- task-15：这条路径（tooltip + 读屏 live region）原来**没有任何断言** ----
  //
  // 原来用户/读屏用户会遇到什么错的：
  // * 悬停顶栏时 tooltip 里是后端的工程散文，`**已在接管默认路由之前中止**`
  //   带着星号；多行靠 `\n`，在 `title` 里时有时无；
  // * 读屏用户的 `.sr-only role="status"` 会把这串文本**逐字念出来**，
  //   包括「星号 星号」与换行 —— 那比视觉乱码更糟：听的人拿不到「这是记号」的线索。
  // 42 个测试文件里没有一条覆盖 `sub`/`detail` 这两个载体 ⇒ 横幅修了、tooltip 没修。
  const GATE_RAW =
    "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应：example.com。\n" +
    "国内网络下「TCP 能连到服务器、代理协议握手被墙」是常见情形。\n" +
    "**已在接管默认路由之前中止**，系统网络未被改动。\n" +
    "**这次探测里失败的全是域名目标**；先试换一个节点。";

  it("前置：这份 fixture 真的带 `**` 与换行（否则下面的断言是空转）", () => {
    expect(GATE_RAW).toContain("**");
    expect(GATE_RAW).toContain("\n");
  });

  it("appStatus 的 sub/detail 不许带 `**` 或换行（它们进 tooltip 与 live region，不是横幅）", () => {
    const st = appStatus({ ...base, corePath: "/xray", lastError: GATE_RAW });
    expect(st.tone).toBe("failed");
    for (const [name, text] of [
      ["sub", st.sub ?? ""],
      ["detail", st.detail],
    ] as const) {
      expect(text, `${name} 里还留着 markdown 记号`).not.toContain("**");
      expect(text, `${name} 里还留着裸换行`).not.toContain("\n");
    }
    // 去掉的是**记号**，不是内容：该说的原因与下一步都还在。
    expect(st.detail).toContain("已在接管默认路由之前中止");
    expect(st.detail).toContain("换一个节点");
  });
});

// ---------------------------------------------------------------------------
// 5. 全局失败横幅（真渲染 App）
// ---------------------------------------------------------------------------

describe("App 全局横幅：人话 + 下一步 + 可点的动作（原来只有一段带 ** 的红字和「关闭」）", () => {
  it("助手类失败：不带 `**`、保留换行、有「去重装助手」入口", async () => {
    // 快照显式给「核心没在跑」：否则顶栏那颗按钮在这一刻是「断开」，
    // 点它只会走 stop（`api.start` 根本不会被调用），断言就测不到这条路径。
    mocks.snapshot.mockResolvedValue(snapshotWith({ runtime: { running: false } }));
    mocks.start.mockRejectedValue({
      code: "helper_protocol_mismatch",
      message:
        "helper 建立 TUN 失败：协议版本不匹配（旧助手）。\n**请重装助手**：设置 → 系统与助手。",
    });
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));

    const reason = await screen.findByText(/helper 建立 TUN 失败/);
    const box = reason.closest(".banner") as HTMLElement;
    const text = box.textContent ?? "";
    expect(text).not.toContain("[object Object]");
    expect(text).not.toContain("**");
    expect(text).toContain("请重装助手");
    expect(text).toContain("下一步");
    expect(within(box).getByRole("button", { name: "去重装助手" })).toBeTruthy();
  });

  it("门禁类失败：「去换一个节点」是真的导航（点完落到节点页）", async () => {
    mocks.snapshot.mockResolvedValue(snapshotWith({ runtime: { running: false } }));
    mocks.start.mockRejectedValue(
      "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应。\n**已在接管默认路由之前中止**。" +
        "先试换一个节点；如果整台 Mac 都上不了网，先点「断开」恢复直连。",
    );
    render(<App />);
    fireEvent.click(await screen.findByRole("button", { name: "连接" }));

    // U8：同屏说清按钮这一刻叫什么。
    await screen.findByText(/顶栏右上角那个按钮现在写的是「连接」/);
    const box = (await screen.findByText(/节点通过了 TCP 检查/)).closest(".banner") as HTMLElement;
    expect(box.textContent).not.toContain("**");

    fireEvent.click(within(box).getByRole("button", { name: "去换一个节点" }));
    // 节点页独有、侧栏没有的东西 —— 证明真的换了页，而不是只改了按钮文案。
    expect(await screen.findByRole("button", { name: "手动添加" })).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// 5b. task-15：tooltip 与读屏 live region（横幅之外的另两个载体）
// ---------------------------------------------------------------------------

describe("顶栏 tooltip 与读屏 live region：后端原文不许带 `**`/换行（task-15）", () => {
  const GATE_RAW =
    "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应：example.com。\n" +
    "**已在接管默认路由之前中止**，系统网络未被改动。\n" +
    "**这次探测里失败的全是域名目标**；先试换一个节点。";

  async function renderTopbarWithLastError() {
    mocks.snapshot.mockResolvedValue(
      snapshotWith({ runtime: { running: false, last_error: GATE_RAW } }),
    );
    const { container } = render(<App />);
    // 顶栏在快照回来后才有 title（含 lastError）。等到它出现为止。
    await vi.waitFor(() => {
      const t = container.querySelector("header.topbar")?.getAttribute("title") ?? "";
      if (!t.includes("已在接管默认路由之前中止")) throw new Error("title 还没更新");
    });
    return container;
  }

  it("`title`（悬停 tooltip）：不带 `**`、不带裸换行，但原因与下一步都还在", async () => {
    const container = await renderTopbarWithLastError();
    const title = container.querySelector("header.topbar")?.getAttribute("title") ?? "";
    expect(title, "tooltip 里还留着 markdown 记号（原来用户看到 `**已在接管默认路由之前中止**`）").not.toContain("**");
    expect(title, "tooltip 里还留着裸换行").not.toContain("\n");
    expect(title).toContain("已在接管默认路由之前中止");
    expect(title).toContain("下一步");
  });

  it("`role=status`（读屏 live region）：不许把「星号 星号」念给用户听", async () => {
    await renderTopbarWithLastError();
    const live = screen.getByRole("status");
    const text = live.textContent ?? "";
    expect(text, "读屏会逐字念出 `**`").not.toContain("**");
    expect(text, "读屏会遇到裸换行").not.toContain("\n");
    // 读屏用户同样要被告知下一步 —— 不能为了去掉记号把这句删掉。
    expect(text).toContain("下一步");
  });

  it("看得到的那份状态文案（仪表盘 sub）也必须同样是干净的", async () => {
    await renderTopbarWithLastError();
    // Dashboard 状态区把同一个 `status.sub` 显示成可见文字。
    const sub = document.querySelector(".dash__state-sub") as HTMLElement | null;
    expect(sub).toBeTruthy();
    expect(sub!.textContent ?? "").not.toContain("**");
    // 整页都不该出现 markdown 记号：这条 fixture 里唯一的 `**` 来源就是 last_error。
    expect(document.body.textContent ?? "").not.toContain("**");
  });
});

// ---------------------------------------------------------------------------
// 6. 意图过滤页：读不到 ≠ 0 条
// ---------------------------------------------------------------------------

describe("Intent：状态读不到时不许显示 0 条 / 未运行 / 没在跑", () => {
  it("intentStatus 读失败 ⇒ 「读不到」而不是「拦截 0 条」", async () => {
    mocks.intentStatus.mockRejectedValue({ reason: "状态锁不可用" });
    const text = await mountIntent(snapshotWith({}), () =>
      (document.body.textContent ?? "").includes("读不到（见上面的错误）"),
    );
    expect(text).toContain("读不到（见上面的错误）");
    expect(text).toContain("状态锁不可用");
    // 原来这三项会显示成 0 / 0 / 未运行 —— 一次 IPC 故障被说成三项确定事实。
    expect(text).not.toContain("拦截 0 条");
    expect(text).not.toContain("未运行");
  });

  it("mitmStatus 读失败 ⇒ 三道闸门与状态表都写「读不到」；不写「没在跑 / 还没生成过」", async () => {
    mocks.mitmStatus.mockRejectedValue({
      kind: "ipc",
      message: "读取 MITM 状态失败（状态锁不可用）",
    });
    const text = await mountIntent(snapshotWith({}), () =>
      (document.body.textContent ?? "").includes("读不到 MITM 状态"),
    );
    expect(text).toContain("读不到 MITM 状态");
    expect(text).toContain("读取 MITM 状态失败（状态锁不可用）");
    expect(text).toContain("下一步");
    // 原来看起来像两项确定事实。
    expect(text).not.toContain("没在跑");
    expect(text).not.toContain("还没生成过");
  });
});

// ---------------------------------------------------------------------------
// 7. MITM 三道闸门 / 默认全关
// ---------------------------------------------------------------------------

describe("MITM 三道闸门：默认全关要一眼看懂", () => {
  it("默认设置（开关关 + 名单空 + 没证书）⇒ 三道都没过，且明说默认全关、不会拆任何域名", async () => {
    const snap = snapshotWith({ mitm: { enabled: false, domains: [], body_strip: null } });
    mocks.mitmStatus.mockResolvedValue(
      mitmStatus({
        enabled: false,
        active: false,
        running: false,
        domains: [],
        ca_fingerprint: null,
        note: "MITM 没开启",
      }),
    );
    const text = await mountIntent(snap, () =>
      (document.body.textContent ?? "").includes("三道闸门都没过"),
    );
    expect(text).toContain("开关 + 名单");
    expect(text).toContain("根证书已装");
    expect(text).toContain("代理在跑");
    expect(text).toContain("三道闸门都没过");
    expect(text).toContain("MITM 默认全关");
    expect(text).toContain("一个域名的 TLS 都不会被拆");

    const gates = mitmGates(snap.settings.mitm, mitmStatus({ enabled: false, active: false, running: false, domains: [], ca_fingerprint: null, note: "MITM 没开启" }));
    expect(gates.map((g) => g.mark)).toEqual(["✗", "✗", "✗"]);
  });

  it("U1：装了证书 + 起了代理但**核心从没启动** ⇒ 不许写「引导规则已随核心生效」", async () => {
    mocks.mitmStatus.mockResolvedValue(
      mitmStatus({ core_steering: null, core_restart_required: false, note: null }),
    );
    const text = await mountIntent(snapshotWith({}), () =>
      (document.body.textContent ?? "").includes("引导规则现在不在任何核心里"),
    );
    expect(text).toContain("核心没在跑 —— 引导规则现在不在任何核心里（先连接核心）");
    expect(text).not.toContain("引导规则已随核心生效");
    // 三道闸门全过，但总结也必须说出核心那一侧缺席。
    expect(text).toContain("此刻没有域名被拆 TLS");
  });

  it("核心正在用旧配置 ⇒ 要重连一次（保留原有结论）", async () => {
    mocks.mitmStatus.mockResolvedValue(
      mitmStatus({ core_steering: false, core_restart_required: true, note: "引导规则要重连一次核心才会生效" }),
    );
    const text = await mountIntent(snapshotWith({}), () =>
      (document.body.textContent ?? "").includes("要重连一次核心才会下发引导规则"),
    );
    expect(text).toContain("要重连一次核心才会下发引导规则");
  });

  it("mitmGates 的 `？` 只出现在「读不到」时（不把读不到说成没过）", () => {
    const snap = snapshotWith({});
    const gates = mitmGates(snap.settings.mitm, null);
    expect(gates.find((g) => g.id === "ca")?.mark).toBe("？");
    expect(gates.find((g) => g.id === "proxy")?.mark).toBe("？");
    expect(mitmGateSummary(gates, null)).toContain("无法判断");
  });
});

// ---------------------------------------------------------------------------
// 8. U2 / U5 / U6 / U13
// ---------------------------------------------------------------------------

describe("Intent：审计与规则的措辞不许把「会」说成「已经」", () => {
  it("U2：列名是「会生成规则」，演练模式下 block 判决显示「不会（演练模式）」且徽章不再绿色「拦截」", async () => {
    mocks.intentAudit.mockResolvedValue([auditRow({ applied: false, outcome: "block" })]);
    await mountIntent();
    const row = (await screen.findByText("ads.example")).closest("tr") as HTMLElement;
    expect(screen.getByText("会生成规则")).toBeTruthy();
    expect(within(row).getByText("不会（演练模式）")).toBeTruthy();
    expect(within(row).getByText("本该拦截（演练）")).toBeTruthy();
    expect(within(row).queryByText("拦截")).toBeNull();
    // 注释里必须说清它**不等于**已下发。
    expect(document.body.textContent).toContain("不代表规则已经下发到核心");
  });

  it("U5：规则行是「当前规则集合」，待下发时写明（消除同屏矛盾）", async () => {
    mocks.intentStatus.mockResolvedValue(summary({ block_rules: 3, rules_pending_apply: true }));
    const text = await mountIntent(snapshotWith({}), () =>
      (document.body.textContent ?? "").includes("（尚未下发到核心）"),
    );
    expect(text).toContain("当前规则集合");
    expect(text).toContain("（尚未下发到核心）");
  });

  it("U6：「闸门」只指 MITM 的三道前提，阈值那一节改叫「命中判据」", async () => {
    const text = await mountIntent();
    expect(text).toContain("命中判据（三条全满足才拦）");
    expect(text).not.toContain("闸门（三条件全满足才拦）");
  });

  it("U13：开关关着时不再说「会生成拦截规则」", async () => {
    const text = await mountIntent(snapshotWith({ intent: { enabled: false, drill: false } }));
    expect(text).toContain("未启用（开关打开后才谈得上）");
    expect(text).not.toContain("关（会生成拦截规则）");
  });
});

// ---------------------------------------------------------------------------
// 9. Dashboard 的 last-error notice（动作真源）
// ---------------------------------------------------------------------------

describe("Dashboard 的「上次运行出错」notice：给出可点动作（原来 rank 0 的红条没有任何按钮）", () => {
  const base = scenarioSnapshot();
  const withError = (msg: string) => ({
    ...base,
    runtime: { ...base.runtime, running: false, last_error: msg },
  });
  const noopRun = (() => Promise.resolve(true)) as never;

  it("助手类 ⇒ 动作「去重装助手」且落到设置页的助手分节", () => {
    const nav = vi.fn();
    const notices = collectNotices(
      withError("helper 建立 TUN 失败：协议版本不匹配（旧助手）") as never,
      noopRun,
      nav,
    );
    const n = notices.find((x) => x.key === "last-error")!;
    expect(n.action?.label).toBe("去重装助手");
    n.action!.run();
    expect(nav).toHaveBeenCalledWith("settings", "set-helper");
  });

  it("门禁类 ⇒ 动作「去换一个节点」，并且原文去 `**`、补一句按钮名", () => {
    const nav = vi.fn();
    const notices = collectNotices(
      withError(
        "节点通过了 TCP 检查，但经它发出的真实请求拿不到响应。\n**已在接管默认路由之前中止**，系统网络未被改动。先点「断开」恢复直连。",
      ) as never,
      noopRun,
      nav,
    );
    const n = notices.find((x) => x.key === "last-error")!;
    expect(n.action?.label).toBe("去换一个节点");
    n.action!.run();
    expect(nav).toHaveBeenCalledWith("nodes");

    const { container } = render(<>{n.text}</>);
    const text = container.textContent ?? "";
    expect(text).not.toContain("**");
    expect(text).toContain("已在接管默认路由之前中止");
    expect(text).toContain("写的是「连接」");
    expect(text).toContain("下一步");
  });
});

// ---------------------------------------------------------------------------
// 10. U4：panic.log 的可发现性
// ---------------------------------------------------------------------------

describe("日志页：崩溃证据 panic.log 必须能被找到（原来 App 内 0 命中）", () => {
  it("常驻一行路径 + 「打开数据目录」动作（复用 api.openDataDir）", async () => {
    mocks.openDataDir.mockResolvedValue(undefined);
    render(
      <StoreProvider>
        <Logs />
      </StoreProvider>,
    );
    await screen.findByRole("button", { name: "打开数据目录" });
    const text = document.body.textContent ?? "";
    expect(text).toContain("panic.log");
    expect(text).toContain(PANIC_LOG_PATH);
    expect(text).toContain("文件:行");

    fireEvent.click(screen.getByRole("button", { name: "打开数据目录" }));
    await vi.waitFor(() => expect(mocks.openDataDir).toHaveBeenCalledTimes(1));
  });
});

/** 把动作数组拼成一句，供「线索 ⇒ 动作」的粗断言使用（避免写死完整文案）。 */
function normalize(steps: string[]): string {
  return steps.join("；");
}
