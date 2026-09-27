/**
 * 「观察结论可持久化 / 可导出」（观察第 1 件事）的界面与源码契约。
 *
 * # 缺陷
 *
 * 结论只活在运行期内存里，App 一重启就没了 —— "看到了什么"没法留档，
 * 也没法拿去做"确定性规则 vs 模型"的判断。而导出/落盘如果失败得悄无声息，
 * 用户会以为自己留好了档。
 *
 * # 判据（本文件覆盖界面 + 源码那一半；Rust 运行期判据见
 * `apps/desktop/src/observe.rs` 的单测，本机不编译 Rust）
 *
 * * 默认关 ⇒ 零摘要且**不产生任何落盘文件**（源码守卫：零摘要早返回）；
 * * 开启 + 命中 ⇒ 计数正确，导出的 JSON **只有摘要、没有正文**；
 * * 域名不在名单 ⇒ 零摘要（负例）；
 * * 导出写到用户给的**绝对路径**；相对路径被拒且给出可读原因；
 * * 「清空」清内存 + 删留档；失败不许静默。
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
  mitmObserveExport: vi.fn(),
  mitmObserveClear: vi.fn(),
  mitmObserveSaved: vi.fn(),
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
      mitmObserveExport: mocks.mitmObserveExport,
      mitmObserveClear: mocks.mitmObserveClear,
      mitmObserveSaved: mocks.mitmObserveSaved,
    },
    subscribe: () => () => {},
  };
});

import Intent from "./pages/Intent";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSettings, MitmStatus, ObserveReport, ObserveReportFile } from "./types";

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

function savedFile(over: Partial<ObserveReportFile> = {}): ObserveReportFile {
  return {
    schema: "xraytun.observe-report.v1",
    app_version: "0.8.45",
    generated_unix: 1_700_000_000,
    started_unix: 1_699_999_000,
    ended_unix: 1_700_000_000,
    enabled: true,
    configured_hosts: ["news.example"],
    observed_hosts: ["news.example"],
    markers: ["promoted"],
    marker_counting: true,
    exchanges: 3,
    marker_total: 4,
    hosts: [],
    capture_body_dir: null,
    note: null,
    privacy: "只含摘要：域名、条数、标记词命中、时间、body 短哈希；不含完整 URL、query 或正文。",
    ...over,
  };
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

async function mount(mitm: MitmStatus, ready: string): Promise<void> {
  mocks.snapshot.mockResolvedValue(
    snapWithObserve({ enabled: true, hosts: ["news.example"], markers: null }),
  );
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
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
  mocks.mitmObserveSaved.mockResolvedValue(null);
});

describe("留档/导出：源码契约（Rust 不在本机编译）", () => {
  it("observe.rs：落档只存摘要；零摘要早返回；导出与留档共用同一个 document()", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("..", "desktop", "src", "observe.rs"), "utf8");
    const prod = src.split("\n#[cfg(test)]\nmod tests")[0] ?? src;

    // 固定留档文件名 + 口径头。
    expect(prod).toContain('pub const OBSERVE_REPORT_FILE: &str = "observe-report.json"');
    expect(prod).toContain("pub const OBSERVE_REPORT_SCHEMA");
    // 唯一的文档构造点：落档与导出都走它。
    expect(prod, "缺 document() 构造点").toContain("pub fn document(");
    // 零摘要 ⇒ 绝不产生文件（判据①的落盘面）。
    expect(prod, "必须有一条「零摘要直接返回」的早返回").toMatch(
      /if ledger\.is_empty\(\)\s*\{\s*return Ok\(path\);/,
    );
    // 隐私口径逐字进文档。
    expect(prod).toContain("privacy: OBSERVE_PRIVACY_NOTE.to_string()");
    // 留档/导出文档的字段里**不许**有承载正文/URL/query 的字段。
    const start = prod.indexOf("pub struct ObserveReportFile {");
    expect(start, "找不到 ObserveReportFile").toBeGreaterThan(-1);
    const block = prod.slice(start, prod.indexOf("\n}", start));
    for (const bad of ["pub body", "pub url", "pub query", "pub payload", "pub content", "pub raw"]) {
      expect(block.includes(bad), `留档文档不许有字段「${bad}」：${block}`).toBe(false);
    }
    expect(block, "口径头必须有版本").toContain("pub app_version");
    expect(block, "口径头必须有 schema").toContain("pub schema");
    expect(block, "口径头必须有起止时间").toContain("pub started_unix");
    expect(block, "口径头必须有观察到的域名").toContain("pub observed_hosts");
  });

  it("commands/mitm.rs：`mitm_apply` 必须把数据目录根交给留档（否则永远不落盘）", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(
      path.resolve("..", "desktop", "src", "commands", "mitm.rs"),
      "utf8",
    );
    const apply = /pub async fn mitm_apply[\s\S]*?\n\}/.exec(src);
    expect(apply, "找不到 mitm_apply").not.toBeNull();
    expect(apply![0], "mitm_apply 必须设置留档路径").toContain("set_report_path_in");
    // 导出 / 清空 / 读留档三个命令都要在。
    expect(src).toContain("pub async fn mitm_observe_export");
    expect(src).toContain("pub async fn mitm_observe_clear");
    expect(src).toContain("pub async fn mitm_observe_saved");
    // 清空必须真的删掉留档文件（不是只清内存）。
    expect(src).toContain("remove_report_file");

    // 导出的绝对路径校验与可读原因在 mitm.rs 的运行态层。
    const runtime = fs.readFileSync(path.resolve("..", "desktop", "src", "mitm.rs"), "utf8");
    expect(runtime).toContain("导出路径必须是绝对路径");
    expect(runtime, "导出与留档必须共用同一个 document()").toContain("archive.document(");
    expect(runtime, "没摘要时要给可读原因").toContain("还没有采到任何摘要");
  });
});

describe("留档/导出：界面行为", () => {
  it("导出与清空入口都在；空路径点击给可读原因且不调后端", async () => {
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "导出");
    expect(document.querySelector("#mitm-observe-export")).not.toBeNull();
    expect(screen.getByRole("button", { name: "导出" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "清空" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "导出" }));
    await waitFor(() => expect(document.body.textContent ?? "").toContain("导出失败"));
    expect(document.body.textContent ?? "").toContain("绝对路径");
    expect(mocks.mitmObserveExport).not.toHaveBeenCalled();
  });

  it("导出成功：把绝对路径原样传给后端，并显示写到的路径", async () => {
    mocks.mitmObserveExport.mockResolvedValue("/Users/you/observe-report.json");
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "导出");

    fireEvent.change(document.querySelector("#mitm-observe-export")!, {
      target: { value: "/Users/you/observe-report.json" },
    });
    fireEvent.click(screen.getByRole("button", { name: "导出" }));

    await waitFor(() => expect(mocks.mitmObserveExport).toHaveBeenCalledWith("/Users/you/observe-report.json"));
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("已导出到 /Users/you/observe-report.json"),
    );
  });

  it("导出失败（相对路径被后端拒）：原因必须显示出来，不是静默", async () => {
    mocks.mitmObserveExport.mockRejectedValue(
      new Error("导出路径必须是绝对路径：relative/leak.json —— 相对路径会落到当前工作目录（可能是仓库）"),
    );
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "导出");

    fireEvent.change(document.querySelector("#mitm-observe-export")!, {
      target: { value: "relative/leak.json" },
    });
    fireEvent.click(screen.getByRole("button", { name: "导出" }));

    await waitFor(() => expect(document.body.textContent ?? "").toContain("导出失败"));
    expect(document.body.textContent ?? "").toContain("当前工作目录");
  });

  it("清空：调后端清空接口并显示回执", async () => {
    mocks.mitmObserveClear.mockResolvedValue(
      mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 0 }),
    );
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "导出");

    fireEvent.click(screen.getByRole("button", { name: "清空" }));
    await waitFor(() => expect(mocks.mitmObserveClear).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(document.body.textContent ?? "").toContain("已清空本会话的观察结论与留档文件"),
    );
  });

  it("上次留档（重启后）能看到：域名、条数与 schema 口径头", async () => {
    mocks.mitmObserveSaved.mockResolvedValue(savedFile());
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "上次留档");
    const text = document.body.textContent ?? "";
    expect(text).toContain("news.example");
    expect(text).toContain("3 条摘要");
    expect(text).toContain("xraytun.observe-report.v1");
  });

  it("上次留档读不出来（文件坏掉）：必须明说，不许静默当成「没有留档」", async () => {
    mocks.mitmObserveSaved.mockRejectedValue(
      new Error("解析观察留档 /x/observe-report.json 失败（文件可能损坏）"),
    );
    await mount(mitmStatus({ enabled: true, configured_hosts: ["news.example"], exchanges: 3 }), "读不到上次留档");
    expect(document.body.textContent ?? "").toContain("读不到上次留档");
    expect(document.body.textContent ?? "").toContain("文件可能损坏");
  });
});
