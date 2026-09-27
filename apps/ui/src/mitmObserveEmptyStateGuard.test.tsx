/**
 * 三空态口径收紧（观察第 3 件事）。
 *
 * # 缺陷
 *
 * `Intent.tsx::observeStatusLine` 的四条分支里，只有「未开启」那条逐字写了
 * 「没有证据 ≠ 干净」。「名单为空」那条（`configured_hosts.length === 0`）**没写**，
 * 只靠页面下方那段固定的盲点清单兜着；「有名单零命中」那条写的是「空不等于干净」，
 * 与另两处不是同一句话。于是同一页里三种"空"的口径不一致 —— 而这一类错误
 * （把"没有证据"讲成"干净"）正是这个功能最危险的失败方式。
 *
 * # 判据
 *
 * 三条空态分支**逐字**都带「没有证据 ≠ 干净」；全页任何分支都不许出现
 * 「未发现广告 / 无广告 / 没有广告。」这类断言。
 *
 * # 为什么有源码守卫
 *
 * 页面下方还有一段固定文案也写着类似的话，DOM 断言可能被它"兜"过去
 * （这正是改前的缺陷形态）。所以除了 DOM，还必须直接扫 `observeStatusLine`
 * 的函数体：三个分支各有一句，一句都不能少。
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

import Intent, { observeStatusLine } from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSettings, MitmStatus, ObserveReport } from "./types";

const PHRASE = "没有证据 ≠ 干净";
const FORBIDDEN = ["未发现广告", "无广告", "没有广告。"];

function report(over: Partial<ObserveReport> = {}): ObserveReport {
  return {
    enabled: false,
    configured_hosts: [],
    markers: ["is_ad", "ad_type", "promoted", "sponsored", "adsbygoogle", "广告"],
    marker_counting: true,
    exchanges: 0,
    marker_total: 0,
    hosts: [],
    capture_body_dir: null,
    note: null,
    ...over,
  };
}

function mitmStatus(reportOver: Partial<ObserveReport> = {}): MitmStatus {
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
  } as MitmStatus;
}

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

async function mountText(snapshot: unknown, mitm: MitmStatus, ready: string): Promise<string> {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.intentStatus.mockResolvedValue(null);
  mocks.intentAudit.mockResolvedValue([]);
  mocks.mitmStatus.mockResolvedValue(mitm);
  render(
    <StoreProvider>
      <Intent />
    </StoreProvider>,
  );
  await screen.findByText("意图过滤");
  await vi.waitFor(() => expect(document.body.textContent ?? "").toContain(ready));
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("三空态：源码守卫（每一条都必须逐字带「没有证据 ≠ 干净」）", () => {
  it("observeStatusLine 的函数体里，三条空态分支各有一句该口径；且不含「未发现广告」式断言", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("src", "pages", "Intent.tsx"), "utf8");
    const m = /export function observeStatusLine\([\s\S]*?\n\}/.exec(src);
    expect(m, "找不到 observeStatusLine（改名了？）").not.toBeNull();
    const body = m![0];

    const hits = (body.match(new RegExp(PHRASE, "g")) ?? []).length;
    expect(
      hits,
      `未开启 / 名单为空 / 有名单零命中 三条空态分支都要逐字带「${PHRASE}」，实际 ${hits} 处`,
    ).toBeGreaterThanOrEqual(3);

    for (const bad of FORBIDDEN) {
      expect(body.includes(bad), `observeStatusLine 里不许出现断言「${bad}」`).toBe(false);
    }
  });

  it("整页源码（生产部分）都不把「没有证据」写成「未发现广告」式结论", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("src", "pages", "Intent.tsx"), "utf8");
    const prod = src.split("export default function Intent()")[0] ?? src;
    for (const bad of FORBIDDEN) {
      expect(prod.includes(bad), `Intent.tsx 生产文案里不许出现「${bad}」`).toBe(false);
    }
  });

  it("纯函数：三条空态的返回值各自都带该口径，且互不相同（口径统一了，原因仍要分开）", () => {
    const unknown = "读不到（见上面的错误）";
    const off = observeStatusLine(report({ enabled: false }), unknown);
    const emptyList = observeStatusLine(report({ enabled: true }), unknown);
    const zeroHit = observeStatusLine(
      report({ enabled: true, configured_hosts: ["a.example"] }),
      unknown,
    );
    for (const [name, line] of [
      ["未开启", off],
      ["名单为空", emptyList],
      ["有名单零命中", zeroHit],
    ] as const) {
      expect(line, `${name} 缺「${PHRASE}」`).toContain(PHRASE);
    }
    expect(new Set([off, emptyList, zeroHit]).size, "三种空态必须是三句不同的话").toBe(3);
  });
});

describe("三空态：DOM 断言（渲染出来的每一句都带口径）", () => {
  it("未开启：DOM 里同时有「观察没开启」与「没有证据 ≠ 干净」", async () => {
    const text = await mountText(
      snapWithObserve({ enabled: false, hosts: [], markers: null }),
      mitmStatus({ enabled: false }),
      "观察没开启",
    );
    expect(text).toContain(PHRASE);
  });

  it("名单为空：DOM 里同时有「名单是空的」与「没有证据 ≠ 干净」", async () => {
    const text = await mountText(
      snapWithObserve({ enabled: true, hosts: [], markers: null }),
      mitmStatus({ enabled: true, configured_hosts: [] }),
      "名单是空的",
    );
    expect(text).toContain(PHRASE);
    expect(text).toContain("一条摘要都不会写");
  });

  it("有名单零命中：DOM 里同时有「还没有采到任何摘要」与「没有证据 ≠ 干净」", async () => {
    const text = await mountText(
      snapWithObserve({ enabled: true, hosts: ["news.example"], markers: null }),
      mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 0 }),
      "还没有采到任何摘要",
    );
    expect(text).toContain(PHRASE);
  });

  it("三空态渲染出来的整页文本都不出现「未发现广告」式断言", async () => {
    const text = await mountText(
      snapWithObserve({ enabled: true, hosts: ["news.example"], markers: null }),
      mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 0 }),
      "还没有采到任何摘要",
    );
    for (const bad of FORBIDDEN) {
      expect(text.includes(bad), `DOM 文本里不许出现「${bad}」`).toBe(false);
    }
  });
});
