/**
 * task-23 D3（+ D2 的意图页部分）：意图页「现在会真的拦吗」不许把「还没下发」答成「会」。
 *
 * # 原来用户会看到什么错的
 *
 * 开关开、演练关、缓存里已有拦截判决（重启 App 会自动加载缓存），但**核心还没重连**
 * （甚至没在跑）时，这一行写的是：
 *
 * > 会：当前有 N 条拦截规则
 *
 * 而同屏另外两处正写着「当前规则集合 …（尚未下发到核心）」与
 * 「判决变了但还没下发给核心」。用户会相信广告已经被拦了 —— 这是这一页唯一一条
 * **把「没发生」说成「已发生」**的陈述。
 *
 * 判据已经存在（`IntentSummary.rules_pending_apply`），所以修法是**纯前端**：
 * 有规则但没下发 ⇒ 独立一态「还不会」，不许匹配 `/^会：/`。
 */
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
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

import Intent, { intentVerdictLine } from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { IntentSummary, MitmStatus } from "./types";

function summary(over: Partial<IntentSummary> = {}): IntentSummary {
  return {
    active: true,
    enabled: true,
    drill: false,
    model: "jev-1.13-free",
    gateway: "https(mock)",
    fingerprint: "abc123",
    pending: 0,
    cache_len: 3,
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

function mitmStatus(over: Partial<MitmStatus> = {}): MitmStatus {
  return {
    enabled: false,
    active: false,
    running: false,
    listen_port: 10810,
    upstream_port: 10811,
    domains: [],
    block_quic: false,
    ca_fingerprint: null,
    ca_expires_at: null,
    stats: null,
    note: "MITM 没开启",
    applied: null,
    core_steering: null,
    core_restart_required: false,
    ...over,
  };
}

function snapshotWithIntent(patch: Record<string, unknown>) {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: { ...base.settings, intent: { ...base.settings.intent, ...patch } },
  };
}

const UNKNOWN = "读不到（见上面的错误）";

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snapshotWithIntent({ enabled: true, drill: false }));
  mocks.tailLogs.mockResolvedValue([]);
  mocks.intentStatus.mockResolvedValue(summary());
  mocks.intentAudit.mockResolvedValue([]);
  mocks.mitmStatus.mockResolvedValue(mitmStatus());
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("intentVerdictLine：唯一判据（纯函数）", () => {
  const on = { enabled: true, drill: false };

  it("有规则但**没下发** ⇒ 「还不会」+「还没下发到核心」（旧写法在这里答「会」）", () => {
    const line = intentVerdictLine(on, summary({ block_rules: 3, rules_pending_apply: true }), UNKNOWN);
    expect(line.startsWith("还不会")).toBe(true);
    expect(line).toContain("还没下发到核心");
    expect(line).toContain("3 条拦截规则已就绪");
    // 最关键的一条：这一态**绝不**匹配「会：」。
    expect(line).not.toMatch(/^会：/);
  });

  it("反例：已下发（`rules_pending_apply === false`）时才允许答「会：」", () => {
    const line = intentVerdictLine(on, summary({ block_rules: 3, rules_pending_apply: false }), UNKNOWN);
    expect(line).toMatch(/^会：/);
    expect(line).toContain("3 条");
  });

  it("0 条规则 ⇒ 「暂时不会」，不是「会」", () => {
    const line = intentVerdictLine(on, summary({ block_rules: 0 }), UNKNOWN);
    expect(line.startsWith("暂时不会")).toBe(true);
    expect(line).not.toMatch(/^会：/);
  });

  it("演练模式 / 开关关 / 读不到 三种态各有各的说法，且都不是「会」", () => {
    expect(intentVerdictLine({ enabled: true, drill: true }, summary({ block_rules: 3 }), UNKNOWN)).toContain(
      "演练模式",
    );
    expect(intentVerdictLine({ enabled: false, drill: false }, summary(), UNKNOWN)).toContain("开关没开");
    const unknown = intentVerdictLine(on, null, UNKNOWN);
    expect(unknown).toContain("读不到");
    expect(unknown).not.toMatch(/^会：/);
  });
});

describe("意图页渲染：这一行必须落在 DOM 上", () => {
  it("待下发时页面文本以「还不会」呈现，且不含「会：当前有」", async () => {
    mocks.intentStatus.mockResolvedValue(summary({ block_rules: 3, rules_pending_apply: true }));
    render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    await screen.findByText(/还不会：3 条拦截规则已就绪/);
    expect(document.body.textContent).not.toContain("会：当前有 3 条拦截规则");
  });

  it("D2：页面级失败横幅是 live region（读屏用户才听得到）", async () => {
    mocks.intentStatus.mockRejectedValue({ reason: "状态锁不可用" });
    render(
      <StoreProvider>
        <Intent />
      </StoreProvider>,
    );
    await screen.findByText(/状态锁不可用/);
    const alert = screen.getAllByRole("alert");
    expect(alert.length).toBeGreaterThan(0);
    expect(alert[0]!.textContent).toContain("状态锁不可用");
  });
});
