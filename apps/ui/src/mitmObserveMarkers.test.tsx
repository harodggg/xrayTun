/**
 * 「标记词表可配置」（观察第 2 件事）的界面与源码契约。
 *
 * # 缺陷
 *
 * 词表写在 `crates/xt-mitm/src/observe.rs::default_markers()` 里，用户改不了。
 * 于是"这个站点的标记词跟我们猜的不一样"这件事无从试验 —— 而观察模式的
 * **全部用途**就是"先采数据，再决定用确定性规则还是模型"。
 *
 * # 本卡选的空词表口径
 *
 * `Some(vec![])` = **明确不统计标记词**（仍按名单采条数 / 短哈希），
 * 报告里 `marker_counting = false`，界面必须**逐字**说"不统计任何标记词"。
 * 不许把"没统计"渲染成"没有命中 / 干净"—— 那正是全 0 的假结论。
 *
 * # 为什么有源码守卫
 *
 * Rust 不在本机编译（门禁只有 vitest）⇒ 后端那一半（`model.rs` 的字段、
 * `observe.rs` 的口径解析）只能扫源码钉住。范式同 `geoTagSourceGuard.test.ts`。
 */
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
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
import type { AppSettings, MitmStatus, ObserveReport } from "./types";

const DEFAULT_MARKERS = ["is_ad", "ad_type", "promoted", "sponsored", "adsbygoogle", "广告"];

function report(over: Partial<ObserveReport> = {}): ObserveReport {
  return {
    enabled: false,
    configured_hosts: [],
    markers: DEFAULT_MARKERS,
    marker_counting: true,
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

async function mount(snapshot: unknown, mitm: MitmStatus): Promise<void> {
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
  await vi.waitFor(() =>
    expect(document.querySelector("#mitm-observe-markers")).not.toBeNull(),
  );
}

function markerBox(): HTMLTextAreaElement {
  const el = document.querySelector("#mitm-observe-markers");
  if (!(el instanceof HTMLTextAreaElement)) throw new Error("找不到标记词表输入框 #mitm-observe-markers");
  return el;
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("标记词表可配置：源码契约（Rust 不在本机编译）", () => {
  it("model.rs：ObserveSettings 有 `markers: Option<Vec<String>>`，且 absent ⇒ None（老 settings 能读）", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(
      path.resolve("..", "..", "crates", "xt-core", "src", "model.rs"),
      "utf8",
    );
    const start = src.indexOf("pub struct ObserveSettings {");
    expect(start, "找不到 ObserveSettings").toBeGreaterThan(-1);
    const block = src.slice(start, src.indexOf("\n}", start));
    expect(block, "词表字段必须存在").toMatch(/pub markers: Option<Vec<String>>/);
    expect(
      block,
      "词表字段必须带 #[serde(default)]（缺省 = None = 用默认词表；老 settings.json 必须能读）",
    ).toMatch(/#\[serde\(default\)\]\s*\n\s*pub markers: Option<Vec<String>>/);
  });

  it("observe.rs：词表按 `Option` 解析（None ⇒ 默认表；Some(空) ⇒ 不统计），并有 marker_counting 口径", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(
      path.resolve("..", "desktop", "src", "observe.rs"),
      "utf8",
    );
    expect(src, "缺省词表的解析函数").toContain("fn effective_markers(");
    // None ⇒ 默认表。
    expect(src, "None 必须落到 default_markers()").toMatch(
      /None\s*=>\s*default_markers\(\)/,
    );
    // 配置装配必须走同一个解析函数（否则界面显示的词表与真正计数用的不是一套）。
    expect(src, "observe_config_for 必须用 effective_markers").toMatch(
      /fn observe_config_for[\s\S]*?markers:\s*effective_markers\(settings\)/,
    );
    // 空表 ⇒ 明确"不统计"，报告里必须有一个显式标志。
    expect(src, "报告必须带 marker_counting 显式标志").toContain("marker_counting");
  });

  it("mitm.rs：词表必须进 digest（否则改完词表点「应用」是空操作）", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("..", "desktop", "src", "mitm.rs"), "utf8");
    expect(src, "digest 必须把生效词表算进去").toMatch(
      /fn digest\([\s\S]*?effective_markers\(settings\)\.join/,
    );
  });

  it("Intent.tsx：有每行一个词的编辑框，且空词表有一句明确的「不统计」说明", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("src", "pages", "Intent.tsx"), "utf8");
    expect(src, "标记词表编辑框").toContain('id="mitm-observe-markers"');
    expect(src, "空词表必须明确说「不统计任何标记词」").toContain("不统计任何标记词");
  });
});

describe("标记词表可配置：界面行为", () => {
  it("设置里的自定义词表按每行一个词显示；失焦保存成数组", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"], markers: ["promoted", "广告"] });
    await mount(snap, mitmStatus({}, { enabled: true, configured_hosts: ["news.example"] }));

    expect(markerBox().value, "显示设置里的自定义词表（每行一个）").toBe("promoted\n广告");

    fireEvent.change(markerBox(), { target: { value: "sponsored\n广告" } });
    fireEvent.blur(markerBox());
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalled());
    const payload = mocks.saveSettings.mock.calls[0]![0] as AppSettings;
    expect(payload.mitm.observe.markers, "保存的是解析后的词数组").toEqual([
      "sponsored",
      "广告",
    ]);
  });

  it("`markers: null`（老设置缺字段）时显示**默认词表**，而不是空白（那不是「不统计」）", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"], markers: null });
    await mount(snap, mitmStatus({}, { enabled: true, configured_hosts: ["news.example"] }));
    expect(markerBox().value).toBe(DEFAULT_MARKERS.join("\n"));
  });

  it("空词表：设置里是空框，且界面明确说「不统计任何标记词」（不许读成「没命中/干净」）", async () => {
    const snap = snapWithObserve({ enabled: true, hosts: ["news.example"], markers: [] });
    await mount(
      snap,
      mitmStatus(
        {},
        {
          enabled: true,
          configured_hosts: ["news.example"],
          markers: [],
          marker_counting: false,
          exchanges: 2,
          marker_total: 0,
          hosts: [
            {
              host: "news.example",
              exchanges: 2,
              marker_total: 0,
              markers: [],
              body_hashes: [],
              last_seen_unix: 1_700_000_000,
            },
          ],
        },
      ),
    );
    expect(markerBox().value, "空词表就是空框").toBe("");
    const text = document.body.textContent ?? "";
    expect(text).toContain("不统计任何标记词");
    expect(text).not.toContain("未发现广告");
  });
});
