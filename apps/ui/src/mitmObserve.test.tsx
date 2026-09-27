/**
 * 「观察（只观察、不改写）」的界面契约。
 *
 * 这一块最危险的错误信念是：**把"没有摘要"读成"没有广告"**。没有证据不是干净。
 * 所以这里的用例专门钉四件事：
 *
 * 1. 默认关 ⇒ 空态必须明说"没有证据 ≠ 干净"，且**不许**出现任何"未发现广告"式结论；
 * 2. 开启 + 域名命中 ⇒ 按域名给出标记词命中汇总（`promoted × 3`），计数按出现次数；
 * 3. 开启但名单空 / 名单非空却零摘要 ⇒ 两种"空"要分开说，且都不是"干净"；
 * 4. 诚实边界与隐私口径必须在页面上（看不到什么 / 摘要里没有什么）。
 */
import { render, screen } from "@testing-library/react";
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

import Intent, { formatMarkerHits, observeStatusLine } from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSettings, MitmStatus, ObserveReport } from "./types";

function snapWithObserve(patch: Partial<AppSettings["mitm"]["observe"]>) {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: {
      ...base.settings,
      mitm: { ...base.settings.mitm, observe: { ...base.settings.mitm.observe, ...patch } },
    },
  };
}

function report(over: Partial<ObserveReport> = {}): ObserveReport {
  return {
    enabled: false,
    configured_hosts: [],
    markers: ["is_ad", "ad_type", "promoted", "sponsored", "adsbygoogle", "广告"],
    exchanges: 0,
    marker_total: 0,
    hosts: [],
    capture_body_dir: null,
    note: null,
    ...over,
  };
}

function mitmStatus(over: Partial<MitmStatus> = {}, reportOver: Partial<ObserveReport> = {}) {
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
    observe: report(reportOver),
    ...over,
  } as MitmStatus;
}

async function mount(
  snapshot: unknown,
  mitm: MitmStatus | Error,
  ready: () => boolean,
): Promise<string> {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.intentStatus.mockResolvedValue(null);
  mocks.intentAudit.mockResolvedValue([]);
  if (mitm instanceof Error) mocks.mitmStatus.mockRejectedValue(mitm);
  else mocks.mitmStatus.mockResolvedValue(mitm);
  render(
    <StoreProvider>
      <Intent />
    </StoreProvider>,
  );
  await screen.findByText("意图过滤");
  await vi.waitFor(() => expect(ready()).toBe(true));
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("MITM 观察：没有证据 ≠ 干净", () => {
  it("默认关：空态必须明说没有证据，且不许出现任何「未发现广告」式结论", async () => {
    const snap = snapWithObserve({ enabled: false, hosts: [] });
    const text = await mount(snap, mitmStatus({ enabled: false, active: false, running: false }), () =>
      (document.body.textContent ?? "").includes("观察没开启"),
    );

    expect(text).toContain("观察没开启");
    expect(text).toContain("不代表没有广告");
    expect(text).toContain("没有证据");
    // 空态**不许**被写成结论性的"干净/无广告"。
    expect(text).not.toContain("未发现广告");
    expect(text).not.toContain("无广告");
    expect(text).not.toContain("没有广告。");
  });

  it("开启 + 域名命中：按域名给出标记词命中汇总（promoted × 3）", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"] });
    const mitm = mitmStatus({}, {
      enabled: true,
      configured_hosts: ["news.example"],
      exchanges: 3,
      marker_total: 4,
      hosts: [
        {
          host: "news.example",
          exchanges: 3,
          marker_total: 4,
          markers: [
            { marker: "promoted", count: 3 },
            { marker: "is_ad", count: 1 },
          ],
          last_seen_unix: 1_700_000_000,
        },
      ],
    });
    const text = await mount(snap, mitm, () =>
      (document.body.textContent ?? "").includes("promoted × 3"),
    );

    expect(text).toContain("news.example");
    expect(text).toContain("promoted × 3");
    expect(text).toContain("is_ad × 1");
    expect(text).toContain("按域名采到 3 条摘要");
  });

  it("开启但名单空：明说一条摘要都不会写（不许说成「干净」）", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: [] });
    const text = await mount(
      snap,
      mitmStatus({}, { enabled: true, configured_hosts: [], exchanges: 0 }),
      () => (document.body.textContent ?? "").includes("名单是空的"),
    );
    expect(text).toContain("名单是空的");
    expect(text).toContain("一条摘要都不会写");
  });

  it("名单非空却零摘要：说「还没采到」并强调空不等于干净", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"] });
    const text = await mount(
      snap,
      mitmStatus({}, { enabled: true, configured_hosts: ["news.example"], exchanges: 0 }),
      () => (document.body.textContent ?? "").includes("还没有采到任何摘要"),
    );
    expect(text).toContain("还没有采到任何摘要");
    expect(text).toContain("空不等于干净");
  });

  it("读不到 MITM 状态：不许把「不知道」渲染成三项确定事实", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"] });
    const text = await mount(snap, new Error("ipc 挂了"), () =>
      (document.body.textContent ?? "").includes("读不到观察结论"),
    );
    expect(text).toContain("读不到观察结论");
    expect(text).toContain("空不等于干净");
  });

  it("隐私口径与盲点清单必须在页面上（摘要没有什么 / 看不到什么）", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"] });
    const text = await mount(snap, mitmStatus(), () =>
      (document.body.textContent ?? "").includes("观察看不到什么"),
    );
    expect(text).toContain("没有完整 URL");
    expect(text).toContain("没有 query");
    expect(text).toContain("没有正文");
    expect(text).toContain("HTTP/2、HTTP/3（QUIC）");
    expect(text).toContain("WebSocket");
    expect(text).toContain("证书固定");
    expect(text).toContain("100-continue");
    expect(text).toContain("12 MiB");
  });

  it("落盘目录被后端拒绝时，原因必须显示出来（不是静默降级）", async () => {
    const snap = snapWithObserve({
      enabled: true,
      hosts: ["news.example"],
      capture_body_dir: "crates/xray-tun/leak",
    });
    const note = "观察的落盘目录必须是绝对路径，crates/xray-tun/leak 会被拒绝（已降级为不落盘）";
    const text = await mount(snap, mitmStatus({}, { enabled: true, note }), () =>
      (document.body.textContent ?? "").includes("已降级为不落盘"),
    );
    expect(text).toContain("已降级为不落盘");
  });

  it("观察的输入控件存在：开关、观察名单、落盘目录各一个", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"] });
    await mount(snap, mitmStatus(), () =>
      (document.body.textContent ?? "").includes("启用观察"),
    );
    expect(document.querySelector("#mitm-observe-enabled")).not.toBeNull();
    expect(document.querySelector("#mitm-observe-hosts")).not.toBeNull();
    expect(document.querySelector("#mitm-observe-dir")).not.toBeNull();
  });
});

describe("观察结论的纯函数判据", () => {
  it("observeStatusLine：未开启 / 名单空 / 零摘要 / 有数据 四种状态各不相同", () => {
    const unknown = "读不到（见上面的错误）";
    expect(observeStatusLine(null, unknown)).toContain("读不到观察结论");
    expect(observeStatusLine(report({ enabled: false }), unknown)).toContain("观察没开启");
    expect(observeStatusLine(report({ enabled: true }), unknown)).toContain("名单是空的");
    expect(
      observeStatusLine(report({ enabled: true, configured_hosts: ["a.example"] }), unknown),
    ).toContain("还没有采到任何摘要");
    const line = observeStatusLine(
      report({ enabled: true, configured_hosts: ["a.example"], exchanges: 2, marker_total: 5 }),
      unknown,
    );
    expect(line).toContain("2 条摘要");
    expect(line).toContain("5 次");
  });

  it("formatMarkerHits：零命中写「无命中」，命中写「词 × 次数」", () => {
    expect(formatMarkerHits([])).toBe("无命中");
    expect(formatMarkerHits([{ marker: "promoted", count: 0 }])).toBe("无命中");
    expect(
      formatMarkerHits([
        { marker: "promoted", count: 3 },
        { marker: "is_ad", count: 0 },
        { marker: "广告", count: 2 },
      ]),
    ).toBe("promoted × 3 · 广告 × 2");
  });
});
