/**
 * 审计自动同步的界面契约（`docs/design/AUDIT-SYNC.md` §8）。
 *
 * # 这一组钉住什么（全是会让用户形成**错误信念**的点）
 *
 * 1. 默认**关闭**，且关闭态不许出现「已开启/现在开着」这类话；
 * 2. `last_ok_unix === null` ⇒ 只能说「从未成功上传过」，**不许**说正常/同步成功；
 * 3. token 没配 ⇒ 开关亮着也**不算配好**，必须给出「服务器会 401」这个明确原因；
 * 4. 非法的 `http://` 地址**存不进去**（按钮禁用 + 写出原因）；
 * 5. 「立即同步」进行中禁用并显示进行中；成功说「上传了 N 天」，失败说原因且
 *    **不出现**成功文案；
 * 6. 「撤回」必须先二次确认，确认后显示**服务端返回的** `deleted`；`error` 非空时
 *    一个字都不许说「已删除」；
 * 7. 预览显示 rows 与明文，并明说「预览不联网」；
 * 8. 意图页那一行在关闭 / 从未成功 / 待传 N 天三种状态下文案正确，且没有点击诱饵。
 *
 * # 怎么桩后端
 *
 * 按任务要求 **mock `invoke`**（`@tauri-apps/api/core`），而不是把整个 `./ipc` 换掉 ——
 * 这样连 `ipc.ts` 里的**命令名**也一起被测到（命令名拼错在 Tauri 里不会报错，
 * 只会静静地拿不到数据）。
 */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn(async () => () => {}) }));

import Settings from "./pages/Settings";
import Intent from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type {
  AuditSyncPreview,
  AuditSyncRevoke,
  AuditSyncRun,
  AuditSyncStatus,
  IntentSummary,
  MitmStatus,
} from "./types";

type Handler = (args?: Record<string, unknown>) => unknown;

/** 本文件的假后端状态（每个测试在 `beforeEach` 里重置）。 */
let auditState: AuditSyncStatus;
const overrides = new Map<string, Handler>();

function status(over: Partial<AuditSyncStatus> = {}): AuditSyncStatus {
  return {
    enabled: false,
    device: "",
    key_present: false,
    token_present: false,
    base_url: "https://xraytun.top",
    last_uploaded_day: null,
    last_ok_unix: null,
    last_attempt_unix: null,
    last_error: null,
    pending_days: [],
    skipped_days: [],
    next_retry_unix: null,
    ...over,
  };
}

function intentSummary(over: Partial<IntentSummary> = {}): IntentSummary {
  return {
    active: true,
    enabled: true,
    drill: true,
    model: "jev-1.13-free",
    gateway: "https(模拟) · jev-1.13-free · 无密钥",
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

function mitmStatus(): MitmStatus {
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
      accepted: 0,
      blocked: 0,
      passed: 0,
      rejected_over_limit: 0,
      failed: 0,
      websocket_refused: 0,
      body_rewritten: 0,
      body_rewrite_declined: 0,
    },
    note: null,
    applied: null,
    core_steering: true,
    core_restart_required: false,
    observe: {
      enabled: false,
      configured_hosts: [],
      markers: ["is_ad"],
      marker_counting: true,
      exchanges: 0,
      marker_total: 0,
      hosts: [],
      capture_body_dir: null,
      note: null,
    },
  };
}

beforeEach(() => {
  vi.clearAllMocks();
  overrides.clear();
  auditState = status();
  mocks.invoke.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    const custom = overrides.get(cmd);
    if (custom) return custom(args);
    switch (cmd) {
      case "snapshot":
        return scenarioSnapshot();
      case "tail_logs":
        return [];
      case "audit_sync_status":
        return { ...auditState };
      case "audit_sync_set_enabled":
        auditState = { ...auditState, enabled: Boolean(args?.enabled) };
        return { ...auditState };
      case "audit_sync_set_base_url": {
        const raw = String(args?.baseUrl ?? "").trim();
        auditState = { ...auditState, base_url: raw === "" ? "https://xraytun.top" : raw };
        return { ...auditState };
      }
      case "audit_sync_set_token":
        auditState = { ...auditState, token_present: String(args?.token ?? "") !== "" };
        return { ...auditState };
      case "audit_sync_now":
        return { uploaded: [], error: null, status: { ...auditState } } satisfies AuditSyncRun;
      case "audit_sync_preview":
        return {
          day: null,
          rows: 0,
          bytes: 0,
          plaintext: null,
          note: "现在没有可预览的天。",
        } satisfies AuditSyncPreview;
      case "audit_sync_revoke":
        return { deleted: 0, error: null } satisfies AuditSyncRevoke;
      case "intent_status":
        return intentSummary();
      case "intent_audit":
        return [];
      case "mitm_status":
        return mitmStatus();
      case "mitm_observe_saved":
        return null;
      default:
        return scenarioSnapshot();
    }
  });
});

/** 打开设置页的「审计同步」那一节，并等状态读回来。 */
async function renderAuditCard() {
  const view = render(
    <StoreProvider>
      <Settings focusSection="set-audit-sync" />
    </StoreProvider>,
  );
  await screen.findByRole("checkbox", { name: /自动同步/ }, { timeout: 4000 });
  return { card: document.getElementById("set-audit-sync") as HTMLElement, view };
}

function auditCard(): HTMLElement {
  return document.getElementById("set-audit-sync") as HTMLElement;
}

const enabledBox = () =>
  screen.getByRole("checkbox", { name: /自动同步/ }) as HTMLInputElement;

describe("审计同步 · 开关与代价", () => {
  it("默认关闭：勾选框是关的，且关闭态只说「开启后会怎样」，不出现「已开启/现在开着」", async () => {
    const { card } = await renderAuditCard();

    expect(enabledBox().checked).toBe(false);
    expect(card.textContent).toContain("开启后每台设备每天自动上传一次");
    expect(card.textContent).toContain("端到端加密的密文");
    expect(card.textContent).toContain("服务器看不到域名");
    expect(card.textContent).toContain("现在关着");
    expect(card.textContent).not.toContain("现在开着");
    expect(card.textContent).not.toContain("已开启");
  });

  it("开启后把代价写清楚：服务端能看到的元数据 + 密钥丢失的后果", async () => {
    auditState = status({
      enabled: true,
      device: "3f2a91c4d0be7715",
      key_present: true,
      token_present: true,
    });
    const { card } = await renderAuditCard();

    expect(card.textContent).toContain("设备随机 id");
    expect(card.textContent).toContain("日期");
    expect(card.textContent).toContain("行数");
    expect(card.textContent).toContain("密文长度");
    expect(card.textContent).toContain("上传时刻");
    expect(card.textContent).toContain("不是");
    expect(card.textContent).toContain("完全匿名");
    expect(card.textContent).toContain("密钥丢失");
    expect(card.textContent).toContain("永久读不出");
  });
});

describe("审计同步 · 状态区（没有证据就不许说成功）", () => {
  it("last_ok_unix 为空 ⇒ 「从未成功上传过」，且不出现「正常/同步成功」", async () => {
    auditState = status({ enabled: true, key_present: true, token_present: true });
    const { card } = await renderAuditCard();

    expect(within(card).getByText(/从未成功上传过/)).toBeTruthy();
    expect(card.textContent).not.toContain("正常");
    expect(card.textContent).not.toContain("同步成功");
    expect(card.textContent).not.toContain("上传成功");
  });

  it("有成功证据时显示传到哪一天；待传天数、跳过天数与退避时间各自成行", async () => {
    auditState = status({
      enabled: true,
      device: "3f2a91c4d0be7715",
      key_present: true,
      token_present: true,
      last_uploaded_day: "2026-09-23",
      last_ok_unix: 1_790_000_000,
      last_attempt_unix: 1_790_000_100,
      last_error: "HTTP 401（上传 token 不对）",
      pending_days: ["2026-09-24", "2026-09-25"],
      skipped_days: ["2026-06-01"],
      next_retry_unix: 1_790_005_000,
    });
    const { card } = await renderAuditCard();

    expect(card.textContent).toContain("传到 2026-09-23");
    expect(card.textContent).toContain("待传天数");
    expect(card.textContent).toContain("2 天");
    expect(card.textContent).toContain("因为太老被跳过");
    expect(card.textContent).toContain("退避中");
    expect(card.textContent).toContain("HTTP 401");
    expect(card.textContent).not.toContain("正常");
  });

  it("设备 id 未初始化时如实说「还没初始化」，不编一个 id", async () => {
    const { card, view } = await renderAuditCard();
    expect(card.textContent).toContain("还没初始化");
    // 反面：有 id 时才给复制按钮与 id 本身。
    expect(within(card).queryByRole("button", { name: "复制设备 id" })).toBeNull();
    view.unmount();

    auditState = status({ device: "3f2a91c4d0be7715" });
    await renderAuditCard();
    expect(screen.getByRole("button", { name: "复制设备 id" })).toBeTruthy();
    expect(auditCard().textContent).toContain("3f2a91c4d0be7715");
  });
});

describe("审计同步 · token 与配置完整度", () => {
  it("token 未配置 ⇒ 开关亮着也不算配好，明确写出「服务器会 401」这个原因", async () => {
    auditState = status({ enabled: true, key_present: true, token_present: false });
    const { card } = await renderAuditCard();

    expect(card.textContent).not.toContain("已配置完成");
    expect(card.textContent).toContain("还没配好");
    expect(card.textContent).toContain("上传 token 还没配置");
    expect(card.textContent).toContain("401");
    // 未配置 ⇒ 给的是**输入框**（不是「已配置（不回显）」）。
    expect(screen.getByLabelText("上传 token")).toBeTruthy();
    expect(card.textContent).not.toContain("已配置（不回显）");
  });

  it("已配置时显示「已配置（不回显）」与「清除」，清除会真的下发空串", async () => {
    auditState = status({ enabled: true, key_present: true, token_present: true });
    const { card } = await renderAuditCard();

    expect(card.textContent).toContain("已配置（不回显）");
    fireEvent.click(within(card).getByRole("button", { name: "清除" }));

    await waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith("audit_sync_set_token", { token: "" }),
    );
    expect(card.textContent).not.toContain("已配置完成");
  });
});

describe("审计同步 · 上传地址校验", () => {
  it("http:// 地址存不进去：按钮禁用、写出原因、点击不下发命令", async () => {
    const { card } = await renderAuditCard();
    const input = screen.getByLabelText(/上传地址/) as HTMLInputElement;

    fireEvent.change(input, { target: { value: "http://audit.example" } });

    const save = within(card).getByRole("button", { name: "保存地址" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(card.textContent).toContain("不能保存");
    expect(card.textContent).toContain("必须以 https:// 开头");

    fireEvent.click(save);
    expect(
      mocks.invoke.mock.calls.some((c) => c[0] === "audit_sync_set_base_url"),
      "非法地址却下发了保存命令",
    ).toBe(false);
  });

  it("合法 https 地址可保存；清空 = 恢复默认（空串）", async () => {
    const { card } = await renderAuditCard();
    const input = screen.getByLabelText(/上传地址/) as HTMLInputElement;

    fireEvent.change(input, { target: { value: "https://audit.example" } });
    fireEvent.click(within(card).getByRole("button", { name: "保存地址" }));
    await waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith("audit_sync_set_base_url", {
        baseUrl: "https://audit.example",
      }),
    );

    fireEvent.change(input, { target: { value: "" } });
    fireEvent.click(within(card).getByRole("button", { name: "恢复默认" }));
    await waitFor(() =>
      expect(mocks.invoke).toHaveBeenCalledWith("audit_sync_set_base_url", { baseUrl: "" }),
    );
  });
});

describe("审计同步 · 立即同步一次", () => {
  it("进行中：按钮禁用并写着「正在同步…」；完成后说清上传了几天", async () => {
    auditState = status({
      enabled: true,
      key_present: true,
      token_present: true,
      pending_days: ["2026-09-24"],
    });
    let release!: () => void;
    const gate = new Promise<void>((r) => {
      release = r;
    });
    overrides.set("audit_sync_now", async () => {
      await gate;
      return {
        uploaded: ["2026-09-24"],
        error: null,
        status: status({
          enabled: true,
          key_present: true,
          token_present: true,
          last_uploaded_day: "2026-09-24",
          last_ok_unix: 1_790_000_000,
        }),
      } satisfies AuditSyncRun;
    });
    const { card } = await renderAuditCard();

    fireEvent.click(within(card).getByRole("button", { name: "立即同步一次" }));

    const busy = (await screen.findByRole("button", { name: "正在同步…" })) as HTMLButtonElement;
    expect(busy.disabled).toBe(true);

    release();
    expect(await screen.findByText(/已上传 1 天/)).toBeTruthy();
    expect(card.textContent).toContain("2026-09-24");
  });

  it("成功：说清「上传了 N 天」并把状态刷新到最新（不再说从未成功）", async () => {
    auditState = status({
      enabled: true,
      key_present: true,
      token_present: true,
      pending_days: ["2026-09-23", "2026-09-24"],
    });
    overrides.set("audit_sync_now", () => ({
      uploaded: ["2026-09-23", "2026-09-24"],
      error: null,
      status: status({
        enabled: true,
        key_present: true,
        token_present: true,
        last_uploaded_day: "2026-09-24",
        last_ok_unix: 1_790_000_000,
      }),
    }) satisfies AuditSyncRun);
    const { card } = await renderAuditCard();

    fireEvent.click(within(card).getByRole("button", { name: "立即同步一次" }));

    expect(await screen.findByText(/已上传 2 天/)).toBeTruthy();
    expect(card.textContent).toContain("2026-09-23、2026-09-24");
    await waitFor(() => expect(card.textContent).not.toContain("从未成功上传过"));
  });

  it("失败：显示后端给的原因，且**不出现**成功文案", async () => {
    auditState = status({
      enabled: true,
      key_present: true,
      token_present: true,
      pending_days: ["2026-09-24"],
    });
    overrides.set("audit_sync_now", () => ({
      uploaded: [],
      error: "上传失败：HTTP 500（服务器内部错误）",
      status: status({
        enabled: true,
        key_present: true,
        token_present: true,
        last_error: "上传失败：HTTP 500（服务器内部错误）",
      }),
    }) satisfies AuditSyncRun);
    const { card } = await renderAuditCard();

    fireEvent.click(within(card).getByRole("button", { name: "立即同步一次" }));

    // 失败原因会在两处出现（本次结果 + 状态区的「最后一次失败的原因」），
    // 所以按卡片文本断言，而不是 `findByText`（后者会因为找到两个而报错）。
    await waitFor(() => expect(card.textContent).toContain("HTTP 500"));
    expect(card.textContent).toContain("同步失败");
    expect(screen.queryByText(/已上传 \d+ 天/)).toBeNull();
  });
});

describe("审计同步 · 撤回（二次确认 + 服务端确认数）", () => {
  it("必须先确认：第一次点击只展开问句，不下发 revoke", async () => {
    const { card } = await renderAuditCard();
    fireEvent.click(within(card).getByRole("button", { name: "撤回全部已上传" }));

    expect(screen.getByText(/确认撤回？/)).toBeTruthy();
    expect(mocks.invoke.mock.calls.some((c) => c[0] === "audit_sync_revoke")).toBe(false);
  });

  it("确认后显示**服务端返回的** deleted 数", async () => {
    overrides.set("audit_sync_revoke", () => ({ deleted: 5, error: null }) satisfies AuditSyncRevoke);
    await renderAuditCard();

    fireEvent.click(screen.getByRole("button", { name: "撤回全部已上传" }));
    fireEvent.click(screen.getByRole("button", { name: "确认撤回" }));

    await waitFor(() => expect(mocks.invoke).toHaveBeenCalledWith("audit_sync_revoke"));
    expect(await screen.findByText(/服务端已确认删除 5 个对象/)).toBeTruthy();
  });

  it("error 非空 ⇒ 只说失败，一个字都不许说已删除", async () => {
    overrides.set("audit_sync_revoke", () => ({
      deleted: 0,
      error: "HTTP 401（上传 token 不对）",
    }) satisfies AuditSyncRevoke);
    await renderAuditCard();

    fireEvent.click(screen.getByRole("button", { name: "撤回全部已上传" }));
    fireEvent.click(screen.getByRole("button", { name: "确认撤回" }));

    expect(await screen.findByText(/撤回失败/)).toBeTruthy();
    expect(screen.getByText(/HTTP 401/)).toBeTruthy();
    expect(screen.queryByText(/服务端已确认删除/)).toBeNull();
    expect(document.body.textContent).not.toContain("已删除");
  });
});

describe("审计同步 · 明文预览（本机数据，不联网）", () => {
  it("显示 day / rows / bytes 与明文 JSON，并写明「预览不联网」", async () => {
    const plaintext = '{"v":1,"kind":"xraytun.intent.audit.day","day":"2026-09-24"}';
    overrides.set("audit_sync_preview", () => ({
      day: "2026-09-24",
      rows: 3,
      bytes: 45678,
      plaintext,
      note: null,
    }) satisfies AuditSyncPreview);
    const { card } = await renderAuditCard();

    fireEvent.click(within(card).getByRole("button", { name: "查看将要上传的内容" }));

    expect(await screen.findByText(/3 行/)).toBeTruthy();
    expect(screen.getByText(/45678 字节/)).toBeTruthy();
    expect(screen.getByText(plaintext)).toBeTruthy();
    expect(card.textContent).toContain("预览不联网");
    expect(card.textContent).toContain("这是本机数据");
  });

  it("没有可预览的天时，显示后端给的那句人话而不是一个空框", async () => {
    overrides.set("audit_sync_preview", () => ({
      day: null,
      rows: 0,
      bytes: 0,
      plaintext: null,
      note: "本地还没有完整的审计天（只上传 day < 今天(UTC) 的完整天）",
    }) satisfies AuditSyncPreview);
    const { card } = await renderAuditCard();

    fireEvent.click(within(card).getByRole("button", { name: "查看将要上传的内容" }));

    expect(await screen.findByText(/本地还没有完整的审计天/)).toBeTruthy();
  });

  it("预览只调预览那一条命令，不会顺手触发上传或撤回", async () => {
    const { card } = await renderAuditCard();
    fireEvent.click(within(card).getByRole("button", { name: "查看将要上传的内容" }));

    await waitFor(() =>
      expect(mocks.invoke.mock.calls.some((c) => c[0] === "audit_sync_preview")).toBe(true),
    );
    expect(mocks.invoke.mock.calls.some((c) => c[0] === "audit_sync_now")).toBe(false);
    expect(mocks.invoke.mock.calls.some((c) => c[0] === "audit_sync_revoke")).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 意图页那一行（与设置页同一份数据源）
// ---------------------------------------------------------------------------

async function renderIntentAudit() {
  render(
    <StoreProvider>
      <Intent />
    </StoreProvider>,
  );
  await screen.findByText("现在是什么状态", undefined, { timeout: 4000 });
  // 那一行先出现（读取中也是这一行），再等它落到真实状态。
  return (await screen.findByText(/审计同步：/, undefined, { timeout: 4000 })) as HTMLElement;
}

describe("意图页 · 审计同步那一行", () => {
  it("关闭：说「关闭」，且不摆任何点击诱饵", async () => {
    const line = await renderIntentAudit();
    await waitFor(() => expect(line.textContent).toContain("审计同步：关闭"));

    expect(line.textContent).toContain("关闭时不会有任何上传请求");
    // 关闭态**没有**可以点的同步按钮（开关/预览/撤回都在设置页）。
    expect(screen.queryByRole("button", { name: /同步/ })).toBeNull();
    expect(screen.queryByRole("button", { name: /上传/ })).toBeNull();
  });

  it("开着但从未成功 ⇒ 「从未成功上传过」，不许显示成正常", async () => {
    auditState = status({ enabled: true, key_present: true, token_present: true });
    const line = await renderIntentAudit();

    await waitFor(() => expect(line.textContent).toContain("从未成功上传过"));
    expect(line.textContent).not.toContain("正常");
  });

  it("待传 N 天 ⇒ 那一行报出天数", async () => {
    auditState = status({
      enabled: true,
      key_present: true,
      token_present: true,
      pending_days: ["2026-09-22", "2026-09-23", "2026-09-24"],
    });
    const line = await renderIntentAudit();

    await waitFor(() => expect(line.textContent).toContain("待传 3 天"));
  });

  it("有成功证据 ⇒ 「已上传到 <day>」", async () => {
    auditState = status({
      enabled: true,
      key_present: true,
      token_present: true,
      last_uploaded_day: "2026-09-23",
      last_ok_unix: 1_790_000_000,
      pending_days: ["2026-09-24"],
    });
    const line = await renderIntentAudit();

    await waitFor(() => expect(line.textContent).toContain("已上传到 2026-09-23"));
    expect(line.textContent).toContain("待传 1 天");
  });
});

// ---------------------------------------------------------------------------
// 浏览器预览兜底（`?preview=1` 的桥接；没有 Tauri 时页面不能崩）
// ---------------------------------------------------------------------------

describe("浏览器预览兜底", () => {
  it("preview 桥接为 audit_sync_* 给出完整默认值，而不是 undefined", async () => {
    const { installPreviewBridge } = await import("./preview");
    const uninstall = installPreviewBridge();
    try {
      const internals = (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ as {
        invoke: (cmd: string, args?: unknown) => Promise<unknown>;
      };

      const s = (await internals.invoke("audit_sync_status")) as AuditSyncStatus;
      expect(s.enabled).toBe(false);
      expect(s.device).toBe("");
      expect(s.pending_days).toEqual([]);
      expect(s.skipped_days).toEqual([]);
      expect(s.last_ok_unix).toBeNull();
      expect(s.base_url).toBe("https://xraytun.top");

      // 写命令也要有反应（预览里点开关不能是"假按钮"）。
      const on = (await internals.invoke("audit_sync_set_enabled", { enabled: true })) as AuditSyncStatus;
      expect(on.enabled).toBe(true);
      expect(on.device).not.toBe("");

      const run = (await internals.invoke("audit_sync_now")) as AuditSyncRun;
      expect(Array.isArray(run.uploaded)).toBe(true);
      expect(run.error).toBeNull();

      const preview = (await internals.invoke("audit_sync_preview", { day: null })) as AuditSyncPreview;
      expect(typeof preview.rows).toBe("number");
      expect(preview.plaintext).not.toBeNull();

      const revoke = (await internals.invoke("audit_sync_revoke")) as AuditSyncRevoke;
      expect(typeof revoke.deleted).toBe("number");
      expect(revoke.error).toBeNull();
    } finally {
      uninstall();
    }
  });
});
