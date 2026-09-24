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
 * 4. **审计里的 `applied` 必须如实显示**（演练模式下是"否"）——
 *    这一列是"这条判决到底有没有生效"的唯一依据。
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
    },
    subscribe: () => () => {},
  };
});

import Intent from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { IntentAuditRecord, IntentSummary } from "./types";

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

async function mount(snapshot: unknown, anchor: string | undefined) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.intentStatus.mockResolvedValue(summary());
  mocks.intentAudit.mockResolvedValue([]);
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
    // 演练模式下 applied=false ⇒ 这一列必须是「否」，不能显示成已生效。
    expect(within(row).getByText("否")).toBeTruthy();
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
});
