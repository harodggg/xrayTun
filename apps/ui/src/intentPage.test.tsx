/**
 * 意图过滤页的界面契约。
 *
 * 挑的都是**会让用户形成错误信念**的点（与 batch2/3/4 的选条标准一致）：
 *
 * 1. **演练模式 ≠ 在拦。** 页面上必须同时看到"演练模式：开（只记录，不下发规则）"
 *    与"生效的规则 拦截 0 条"。只显示一句"已开启"会让人以为广告已经被拦了。
 * 2. **待生效必须说人话，并且只由用户点。** 规则在核心启动时才下发，
 *    所以"应用（会重连一次）"这个按钮的存在与否，取决于 `rules_pending_apply`；
 *    页面上不能出现任何自动重连的路径。
 * 3. **缺密钥要在"还差什么"里说清楚**，而不是让用户对着一个永远不动的引擎猜。
 * 4. **审计里的 `applied` 必须如实显示**（演练模式下是"不会（演练模式）"）——
 *    这一列是"这条判决会不会构成规则"的唯一依据。task-3 起列名从「生效」改成
 *    「会生成规则」：`applied` 与「已下发到核心」无关（engine.rs:444-446）。
 */
import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
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
      saveSettings: mocks.saveSettings,
      tailLogs: vi.fn().mockResolvedValue([]),
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

import Intent from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { IntentAuditRecord, IntentSummary, MitmStatus } from "./types";

function mitmStatus(over: Partial<MitmStatus> = {}): MitmStatus {
  return {
    enabled: true,
    active: true,
    running: true,
    listen_port: 10810,
    upstream_port: 10811,
    domains: ["ads.example"],
    block_quic: false,
    ca_fingerprint: "AA:BB",
    ca_expires_at: "2028-09-25",
    stats: {
      accepted: 3,
      blocked: 2,
      passed: 1,
      rejected_over_limit: 0,
      failed: 0,
      websocket_refused: 0,
      body_rewritten: 1,
      body_rewrite_declined: 2,
    },
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
    gateway: "https(mock) · jev-1.13-free · 无密钥",
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

function snapshotWithIntent(patch: Record<string, unknown>) {
  const base = scenarioSnapshot();
  return { ...base, settings: { ...base.settings, intent: { ...base.settings.intent, ...patch } } };
}

async function mount(
  snapshot: unknown,
  anchor: string | undefined,
  mitm: MitmStatus = mitmStatus(),
) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.intentStatus.mockResolvedValue(summary());
  mocks.intentAudit.mockResolvedValue([]);
  mocks.mitmStatus.mockResolvedValue(mitm);
  render(
    <StoreProvider>
      <Intent />
    </StoreProvider>,
  );
  if (anchor !== undefined) {
    await screen.findByText(anchor);
  }
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("意图过滤页", () => {
  it("演练模式必须同时显示「只记录」与「拦截 0 条」，而不是一句「已开启」", async () => {
    const text = await mount(snapshotWithIntent({ enabled: true, drill: true }), "现在是什么状态");
    expect(text).toContain("演练模式");
    expect(text).toContain("开（只记录，不下发规则）");
    expect(text).toContain("拦截 0 条");
  });

  it("有待生效的规则时给出「应用（会重连一次）」；没有时不给按钮", async () => {
    mocks.intentStatus.mockResolvedValue(summary({ rules_pending_apply: true }));
    mocks.snapshot.mockResolvedValue(snapshotWithIntent({ enabled: true }));
    mocks.intentAudit.mockResolvedValue([]);
    mocks.mitmStatus.mockResolvedValue(mitmStatus());
    const { unmount } = render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    await screen.findByText("现在是什么状态");
    const apply = await screen.findByRole("button", { name: /应用（会重连一次）/ });
    expect(apply).toBeTruthy();
    unmount();

    // 没有待生效 ⇒ 按钮不存在（它不该变成一个"平时也能点"的按钮）。
    mocks.intentStatus.mockResolvedValue(summary({ rules_pending_apply: false }));
    render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    await screen.findByText("现在是什么状态");
    expect(screen.queryByRole("button", { name: /应用（会重连一次）/ })).toBeNull();
  });

  it("未开启时要在「还差什么」里说清楚", async () => {
    await mount(snapshotWithIntent({ enabled: false }), undefined);
    // 锚点用正则：`findByText` 默认是**整串相等**，而标题是"要让它真的能拦，还差什么"。
    await screen.findByText(/还差什么/);
    expect(await screen.findByText("意图过滤未开启")).toBeTruthy();
  });

  it("需要密钥但没配时，给出的下一步是「改用 Zen」而不是一句报错", async () => {
    await mount(
      snapshotWithIntent({ enabled: true, preset: "typesafe", api_key_ref: "" }),
      undefined,
    );
    const hint = await screen.findByText(/想零密钥试水请改用 Zen 预设/);
    expect(hint.textContent).toContain("Jev API Key");
  });

  it("审计里的 applied 如实显示，且「为什么」能给出判据", async () => {
    mocks.intentStatus.mockResolvedValue(summary({ cache_len: 1, blocked: 1 }));
    mocks.intentAudit.mockResolvedValue([auditRow({ applied: false })]);
    mocks.intentExplain.mockResolvedValue({
      host: "ads.example",
      verdict: {
        verdict: "block",
        category: "ad_or_monetization",
        ads_intent: 0.97,
        risk_of_breakage: 0.05,
        choice_confidence: 0.93,
        effective_min: 0.85,
      },
      decided_at_unix: 1_700_000_000,
      expires_at_unix: 1_800_000_000,
      hits: 3,
      model: "jev-latest",
    });
    mocks.snapshot.mockResolvedValue(snapshotWithIntent({ enabled: true }));
    render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );

    const row = (await screen.findByText("ads.example")).closest("tr")!;
    // U2（task-3）：这一列的判据是 `!drill && verdict.is_block()`（engine.rs:444-446），
    // 只说明「这条判决构成一条拦截规则」，与「是否已下发到核心」无关 ⇒
    // 演练模式下显示「不会（演练模式）」，并且结论徽章不再是绿色「拦截」
    // （绿色在这套配色里 = 已完成，会读成「正在被拦」）。
    expect(within(row).getByText("不会（演练模式）")).toBeTruthy();
    expect(within(row).getByText("本该拦截（演练）")).toBeTruthy();
    expect(within(row).queryByText("拦截")).toBeNull();
    expect(within(row).getByText("0.97")).toBeTruthy();

    (within(row).getByRole("button", { name: "为什么" }) as HTMLButtonElement).click();
    await screen.findByText(/ads.example 的判决/);
    expect(mocks.intentExplain).toHaveBeenCalledWith("ads.example");
  });

  it("放行按钮把动作显式传下去（我们绝不替用户选直连还是走代理）", async () => {
    mocks.intentStatus.mockResolvedValue(summary({ cache_len: 1 }));
    mocks.intentAudit.mockResolvedValue([auditRow({ outcome: "block" })]);
    mocks.intentAllow.mockResolvedValue(summary());
    mocks.snapshot.mockResolvedValue(snapshotWithIntent({ enabled: true }));
    render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    const row = (await screen.findByText("ads.example")).closest("tr")!;
    (within(row).getByRole("button", { name: "放行（直连）" }) as HTMLButtonElement).click();
    await vi.waitFor(() => expect(mocks.intentAllow).toHaveBeenCalledWith("ads.example", "direct"));
  });

  // ---- MITM（内容级判定）------------------------------------------------
  //
  // 这几条挑的都是**会让用户形成错误信念**的点：以为"开了就在拆包/就在拦"、
  // 以为"装了证书就生效了"、以为"空名单也没关系"。

  it("MITM 的「为什么没生效」由后端给的一句话显示，而不是界面自己猜", async () => {
    const text = await mount(
      scenarioSnapshot(),
      "MITM（内容级判定，可选）",
      mitmStatus({
        running: false,
        note: "根证书还没装进系统钥匙串：引导规则不会下发给核心，HTTPS 照常直连",
      }),
    );
    expect(text).toContain("根证书还没装进系统钥匙串");
    // 没在跑就必须显示"没在跑"，不能因为开关开着就显示成生效。
    expect(text).toContain("没在跑");
  });

  it("证书刚装好但核心还没重连时，必须明说「要重连一次核心」", async () => {
    const text = await mount(
      scenarioSnapshot(),
      "MITM（内容级判定，可选）",
      mitmStatus({ core_restart_required: true, core_steering: false }),
    );
    expect(text).toContain("要重连一次核心才会下发引导规则");
  });

  it("名单为空时必须说「不会拆任何域名」，而不是让用户以为开关=在拆包", async () => {
    const base = scenarioSnapshot();
    mocks.snapshot.mockResolvedValue({
      ...base,
      settings: {
        ...base.settings,
        mitm: { ...base.settings.mitm, enabled: true, domains: [] },
      },
    });
    mocks.mitmStatus.mockResolvedValue(mitmStatus({ active: false, domains: [], running: false }));
    const { unmount } = render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    await screen.findByText("MITM（内容级判定，可选）");
    expect(document.body.textContent).toContain("空（不会拆任何域名）");
    unmount();
  });

  it("「装入根证书」只调装证书那一个命令（那是唯一改系统状态的动作，必须可归因）", async () => {
    mocks.mitmInstallCa.mockResolvedValue(mitmStatus());
    mocks.mitmRemoveCa.mockResolvedValue(mitmStatus({ ca_fingerprint: null }));
    mocks.mitmApply.mockResolvedValue(mitmStatus());
    await mount(scenarioSnapshot(), "MITM（内容级判定，可选）");
    (screen.getByRole("button", { name: "装入根证书" }) as HTMLButtonElement).click();
    await vi.waitFor(() => expect(mocks.mitmInstallCa).toHaveBeenCalledTimes(1));
    expect(mocks.mitmApply).not.toHaveBeenCalled();
    expect(mocks.mitmRemoveCa).not.toHaveBeenCalled();
  });

  it("「应用（起/停代理）」只调起代理那一个命令", async () => {
    mocks.mitmApply.mockResolvedValue(mitmStatus());
    await mount(scenarioSnapshot(), "MITM（内容级判定，可选）");
    (screen.getByRole("button", { name: "应用（起/停代理）" }) as HTMLButtonElement).click();
    await vi.waitFor(() => expect(mocks.mitmApply).toHaveBeenCalledTimes(1));
    expect(mocks.mitmInstallCa).not.toHaveBeenCalled();
  });

  it("代理的账要如实显示（含「裁剪未生效」这一列，它是最容易沉默失败的一项）", async () => {
    const text = await mount(scenarioSnapshot(), "MITM（内容级判定，可选）");
    expect(text).toContain("阻断 2");
    expect(text).toContain("裁剪生效 1");
    expect(text).toContain("裁剪未生效 2");
  });
});
