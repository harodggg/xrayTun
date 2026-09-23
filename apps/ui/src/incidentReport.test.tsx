/**
 * task-131：「报告问题」链路的前端测试。
 *
 * 覆盖卡里点名的四条上传路径 + 隐私红线 + 复制反馈 + 哨兵角标：
 *
 * | 场景 | 注入 | 断言 |
 * |---|---|---|
 * | 201 成功 | `incidentUpload` 返回 `{id,…}` | 显示编号 + 「复制编号」出现 |
 * | 422 疑似密钥 | reject `{kind:"secret_detected", hits:[…]}` | 「已阻止上传」+ `文件:行号:类型`，**不回显密钥原文** |
 * | 网络失败 | reject `{kind:"network"}` | 说清 + 给出下一步 |
 * | 5xx | reject `{kind:"server", code:503}` | 说清 + 带状态码 + 给出下一步 |
 * | 隐私红线 | —— | 预览渲染之前**不存在**「确认上传」，且 `incidentUpload` 一次都没被调用 |
 * | 复制失败 | `clipboard.writeText` reject | `role="alert"` + 可手动选中的 textarea |
 * | 哨兵角标 | `incidentAnomalyCount` = 0 / 3 | 0 不显示、3 显示「有 3 条待上报」 |
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  incidentPreview: vi.fn(),
  incidentUpload: vi.fn(),
  incidentAnomalyCount: vi.fn(),
  diagnostics: vi.fn(),
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    incidentPreview: mocks.incidentPreview,
    incidentUpload: mocks.incidentUpload,
    incidentAnomalyCount: mocks.incidentAnomalyCount,
    diagnostics: mocks.diagnostics,
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import IncidentReport, { CopyButton } from "./IncidentReport";
import { formatServerTime } from "./incident";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

const BUNDLE = "/tmp/xraytun-incident-x.tar.gz";

const PREVIEW = {
  bundle_path: BUNDLE,
  size_bytes: 2048,
  files: [
    { name: "logs/app.jsonl", bytes: 1024, sha256: "a".repeat(64) },
    { name: "state.json", bytes: 1024, sha256: "b".repeat(64) },
  ],
  readme: "已抹掉订阅 URL 里的凭据与 UUID 形状的 token。",
  manifest: "…",
  truncated: ["logs/app.jsonl"],
};

/** 假的「密钥原文」：断言它**绝不**出现在界面上。 */
const FAKE_SECRET = "8faf00f8-2ffd-470e-81d1-f0f7c9b50b58";

function setClipboard(impl: (text: string) => Promise<void>) {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText: vi.fn(impl) },
    configurable: true,
  });
  return navigator.clipboard.writeText as unknown as ReturnType<typeof vi.fn>;
}

async function renderPanel() {
  const r = render(<IncidentReport />);
  // 等哨兵计数那一次被动读取落地
  await waitFor(() => expect(mocks.incidentAnomalyCount).toHaveBeenCalled());
  return r;
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.incidentAnomalyCount.mockResolvedValue(0);
  mocks.incidentPreview.mockResolvedValue(PREVIEW);
  mocks.incidentUpload.mockResolvedValue({
    id: "inc-201",
    sha256: "c".repeat(64),
    bytes: 2048,
    // ISO 8601 串（不是 unix 秒）—— 与 `src/worker.mjs:263` 的契约一致
    received_at: "2026-09-23T03:29:48.123Z",
  });
  setClipboard(() => Promise.resolve());
});

describe("task-131 · 隐私红线：必须先看清单才有「确认上传」", () => {
  it("进入时既没有清单也没有「确认上传」，且**一次都没**调用上传", async () => {
    await renderPanel();
    expect(screen.queryByRole("button", { name: "确认上传" })).toBeNull();
    expect(screen.queryByText("包内清单")).toBeNull();
    expect(mocks.incidentUpload).not.toHaveBeenCalled();
  });

  it("点「报告问题」⇒ 只调 preview，然后清单/脱敏说明/被截断项都出现，此时才有「确认上传」", async () => {
    await renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "报告问题" }));
    await screen.findByText("包内清单");

    expect(mocks.incidentPreview).toHaveBeenCalledTimes(1);
    expect(mocks.incidentUpload, "预览阶段绝不能上传").not.toHaveBeenCalled();
    // 清单三件套
    // 文件名在两处出现（清单里 + 「被截断」那一行里），所以用 getAll
    expect(screen.getAllByText("logs/app.jsonl").length).toBeGreaterThan(0);
    expect(screen.getByText("state.json")).toBeTruthy();
    expect(screen.getByText(/已抹掉订阅 URL 里的凭据/)).toBeTruthy();
    expect(screen.getByText(/因为太大被截断/)).toBeTruthy();
    // 到这里才出现
    expect(await screen.findByRole("button", { name: "确认上传" })).toBeTruthy();
  });

  it("反例：preview 失败 ⇒ 不出现「确认上传」，也不会上传", async () => {
    mocks.incidentPreview.mockRejectedValue(new Error("打包失败"));
    await renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "报告问题" }));
    await screen.findByRole("alert");
    expect(screen.queryByRole("button", { name: "确认上传" })).toBeNull();
    expect(mocks.incidentUpload).not.toHaveBeenCalled();
  });
});

describe("task-131 · 四条上传路径", () => {
  const toPreview = async () => {
    await renderPanel();
    fireEvent.click(screen.getByRole("button", { name: "报告问题" }));
    return screen.findByRole("button", { name: "确认上传" });
  };

  it("201：上传成功 ⇒ 显示编号 + 「复制编号」", async () => {
    const confirm = await toPreview();
    fireEvent.click(confirm);
    await screen.findByText(/已上传/);
    expect(mocks.incidentUpload).toHaveBeenCalledWith(BUNDLE);
    expect(screen.getByText("inc-201")).toBeTruthy();
    expect(screen.getByRole("button", { name: "复制编号" })).toBeTruthy();
    // `received_at` 是 ISO 串 ⇒ 必须真的渲染出「服务器时间 …」（不是永远不显示）
    expect(screen.getByText(new RegExp(`服务器时间 ${formatServerTime("2026-09-23T03:29:48.123Z")}`))).toBeTruthy();
  });

  it("反例：received_at 解析不出来 ⇒ 一个时间都不编", async () => {
    mocks.incidentUpload.mockResolvedValue({
      id: "inc-202",
      sha256: "d".repeat(64),
      bytes: 2048,
      received_at: "不是时间",
    });
    fireEvent.click(await toPreview());
    await screen.findByText(/已上传/);
    expect(screen.queryByText(/服务器时间/)).toBeNull();
    // 编号照旧要显示 —— 缺时间不影响主信息
    expect(screen.getByText("inc-202")).toBeTruthy();
  });

  it("422：SecretDetected ⇒ 已阻止上传 + `文件:行号:类型`，且**不回显密钥原文**", async () => {
    mocks.incidentUpload.mockRejectedValue({
      kind: "secret_detected",
      // 后端 message 里**可能**夹带密钥；界面一个字都不许渲染它
      message: `found ${FAKE_SECRET} in logs/app.jsonl`,
      hits: [{ file: "logs/app.jsonl", line: 42, kind: "uuid" }],
    });
    const confirm = await toPreview();
    fireEvent.click(confirm);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("已阻止上传");
    expect(alert.textContent).toContain("logs/app.jsonl:42:uuid");
    expect(alert.textContent, "只给位置与类型，不许回显密钥").not.toContain(FAKE_SECRET);
    expect(document.body.textContent).not.toContain(FAKE_SECRET);
    // 失败后包还在本地：清单仍在，可以重试
    expect(screen.getByRole("button", { name: "确认上传" })).toBeTruthy();
  });

  it("网络失败 ⇒ 说清原因 + 给出下一步（且本地包还在）", async () => {
    mocks.incidentUpload.mockRejectedValue({ kind: "network" });
    fireEvent.click(await toPreview());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("网络");
    expect(alert.textContent).toContain("本地包还在");
  });

  it("5xx ⇒ 带状态码 + 给出下一步", async () => {
    mocks.incidentUpload.mockRejectedValue({ kind: "server", code: 503, message: "upstream down" });
    fireEvent.click(await toPreview());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("HTTP 503");
    expect(alert.textContent).toContain("隔一会儿重试");
  });

  it("限流 ⇒ 说清「过频」与「包不会丢」", async () => {
    mocks.incidentUpload.mockRejectedValue("RateLimited");
    fireEvent.click(await toPreview());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("过频");
    expect(alert.textContent).toContain("不会丢");
  });
});

describe("task-131 · 复制失败必须可见（不许静默）", () => {
  it("剪贴板被拒 ⇒ role=alert + 可手动选中的 textarea 里就是那段文本", async () => {
    setClipboard(() => Promise.reject(new Error("NotAllowedError")));
    render(<CopyButton label="复制编号" text="inc-201" />);
    fireEvent.click(screen.getByRole("button", { name: "复制编号" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("复制失败");
    expect(alert.textContent).toContain("剪贴板不可用");
    const box = screen.getByLabelText("手动复制内容") as HTMLTextAreaElement;
    expect(box.value).toBe("inc-201");
    expect(box.readOnly).toBe(true);
    expect(screen.queryByText(/已复制到剪贴板/)).toBeNull();
  });

  it("反例：剪贴板成功 ⇒ 给 role=status 的确认，且不出现失败块", async () => {
    setClipboard(() => Promise.resolve());
    render(<CopyButton label="复制编号" text="inc-201" />);
    fireEvent.click(screen.getByRole("button", { name: "复制编号" }));
    expect(await screen.findByText(/已复制到剪贴板/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByLabelText("手动复制内容")).toBeNull();
  });

  it("取文本失败（load 抛错）也可见：不会静默什么都不发生", async () => {
    setClipboard(() => Promise.resolve());
    render(<CopyButton label="复制诊断报告" load={() => Promise.reject(new Error("读日志失败"))} />);
    fireEvent.click(screen.getByRole("button", { name: "复制诊断报告" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("生成要复制的文本失败");
    expect(alert.textContent).toContain("读日志失败");
  });
});

describe("task-131 · 设置页那个旧的「复制诊断报告」也接上了可见反馈", () => {
  it("剪贴板被拒 ⇒ 设置页里出现 role=alert 与可手动选中的文本区", async () => {
    setClipboard(() => Promise.reject(new Error("NotAllowedError")));
    const settings = await import("./pages/Settings");
    mocks.snapshot.mockResolvedValue(scenarioSnapshot());
    mocks.tailLogs.mockResolvedValue([]);
    mocks.diagnostics.mockResolvedValue("（诊断报告正文）");
    render(
      <StoreProvider>
        <settings.default focusSection="set-misc" />
      </StoreProvider>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "复制诊断报告" }));
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("复制失败");
    expect((screen.getByLabelText("手动复制内容") as HTMLTextAreaElement).value).toBe(
      "（诊断报告正文）",
    );
  });
});

describe("task-131 · 被动哨兵角标只看本地计数", () => {
  it("计数 0 ⇒ 不显示角标", async () => {
    mocks.incidentAnomalyCount.mockResolvedValue(0);
    await renderPanel();
    expect(screen.queryByText(/条待上报/)).toBeNull();
  });

  it("计数 3 ⇒ 显示「有 3 条待上报」，且**没有**触发采集或上传", async () => {
    mocks.incidentAnomalyCount.mockResolvedValue(3);
    await renderPanel();
    expect(screen.getByText("有 3 条待上报")).toBeTruthy();
    expect(mocks.incidentPreview).not.toHaveBeenCalled();
    expect(mocks.incidentUpload).not.toHaveBeenCalled();
  });

  it("反例：读不到计数（命令缺席/报错）⇒ 不显示角标，也不崩", async () => {
    mocks.incidentAnomalyCount.mockRejectedValue(new Error("no such command"));
    render(<IncidentReport />);
    await screen.findByRole("button", { name: "报告问题" });
    expect(screen.queryByText(/条待上报/)).toBeNull();
  });
});
