/**
 * 「设置 → 内核与更新」一屏的可读性修复（评审清单剩项，DOM 判据）。
 *
 * # 这一屏改前的具体毛病（用户看到的）
 *
 * | # | 改前 | 用户看到什么 |
 * |---|---|---|
 * | 1 | `Settings.tsx` 里客户端 / 核心 / geo 三颗安装按钮都是 `btn--primary` | 三块同样重的蓝按钮并排，像「必须都点」；「检查更新」「回退」夹在中间分不出主次 |
 * | 3 | 核心路径是 `.field__hint` 里的一段纯文本 | 长路径没法一键复制、也不等宽；提示里的长串直接把卡片撑破 |
 * | 4 | 内核一节的 TUN 说明**无条件**写「建议使用最新的 26.9.x」 | 已经装了支持原生 TUN 的核心（`supports_native_tun === true`）也照样被劝升级 |
 * | 5 | `UpdateBar` 是一个纯 `<div>` 画出来的进度条 | 读屏完全读不到「正在下载 / 进度多少」 |
 * | 6 | 关键事实与开发者细节**混在同一段提示**里，且开发者细节没有折叠 | 用户要在配额 / SHA256SUMS / 日志路径里翻找「会不会动 App 包」「要不要先断开」 |
 *
 * # 判据为什么这么写
 *
 * 本仓的 UI 测试跑在 **jsdom**（CSS 不加载、没装 `jest-dom`），所以：
 * * 「主按钮数量」用 `button.btn--primary` 的 DOM 计数（类名来自源码，不依赖 CSS 生效）；
 * * 「等宽」断言 `.input.mono` 这个类名真的挂上了（外观一致性由 `updateSourceGuard.test.ts` 扫 CSS 保证）；
 * * 「可见」用 `closest("details")` 判定 —— jsdom 不实现 `<details>` 的可见性计算，
 *   所以口径是「**不在收起的 `<details>` 里**」而不是 `checkVisibility()`。
 *
 * 每条判据都在改前跑过并红（见提交报告），不是事后补的。
 */
import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ snapshot: vi.fn() }));

vi.mock("./ipc", async () => {
  const actual = await vi.importActual<typeof import("./ipc")>("./ipc");
  return {
    ...actual,
    api: { ...actual.api, snapshot: mocks.snapshot },
    // StoreProvider 会订阅事件；真实 subscribe 会走 Tauri API，在 jsdom 里没有。
    subscribe: () => () => {},
  };
});

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSnapshot } from "./types";

type Update = AppSnapshot["update"];

/** 造一条「GitHub 上有这个版本」的条目（字段与真实 release 同形）。 */
function release(version: string, prerelease = false): NonNullable<Update["latest_core"]> {
  return {
    size: 12_345_678,
    version,
    published_at: "2026-09-20T16:03:44Z",
    prerelease,
    download_url: `https://github.com/XTLS/Xray-core/releases/download/${version}/Xray-${version}.zip`,
    digest_url: null,
  };
}

/** 预览快照打底，只替换要用到的两段。 */
function snap(over: { update?: Partial<Update>; core?: Partial<AppSnapshot["core"]> } = {}): AppSnapshot {
  const base = scenarioSnapshot();
  return {
    ...base,
    core: { ...base.core, ...over.core },
    update: { ...base.update, ...over.update },
  };
}

function renderAt(s: AppSnapshot, section: string) {
  mocks.snapshot.mockResolvedValue(s);
  return render(
    <StoreProvider>
      <Settings focusSection={section} />
    </StoreProvider>,
  );
}

/** 落在「核心与数据更新」这一节（等它真的渲染出来）。 */
async function renderUpdate(s: AppSnapshot) {
  const r = renderAt(s, "set-update");
  await screen.findByRole("button", { name: "检查更新" });
  const el = document.querySelector<HTMLElement>("#set-update");
  expect(el, "#set-update 必须在 DOM 里（否则下面的断言会落空）").not.toBeNull();
  return { r, section: el! };
}

function primaries(root: ParentNode): HTMLButtonElement[] {
  return [...root.querySelectorAll<HTMLButtonElement>("button.btn--primary")];
}

/** 收起的 `<details>` 祖先（null = 这个元素没有被折叠藏起来）。 */
function closedDetailsAncestor(el: Element | null): Element | null {
  let cur: Element | null = el;
  while (cur) {
    if (cur.tagName === "DETAILS" && !(cur as HTMLDetailsElement).open) return cur;
    cur = cur.parentElement;
  }
  return null;
}

/** 让 `CopyButton` 的剪贴板有一个确定的实现（jsdom 默认没有它）。 */
function setClipboard(impl: (text: string) => Promise<void>) {
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText: vi.fn(impl) },
    configurable: true,
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  // 默认成功；失败分支的用例自己覆盖。
  setClipboard(() => Promise.resolve());
});

// ---------------------------------------------------------------------------
// 1) 一屏一主按钮
// ---------------------------------------------------------------------------

describe("1 · 一屏一主按钮：客户端 > 核心 > geo", () => {
  it("三条通道都有新版 ⇒ 恰好 1 个 `.btn--primary`，且是**客户端**那颗（优先级最高）", async () => {
    const { section } = await renderUpdate(
      snap({
        update: {
          latest_app: release("0.8.99"),
          app_update_available: true,
          check_error_app: null,
          latest_core: release("v26.9.10", true),
          core_update_available: true,
          latest_geo: release("v26.9.10"),
        },
      }),
    );

    const primary = primaries(section);
    expect(primary, "三颗蓝按钮并排 ⇒ 用户以为必须都点").toHaveLength(1);
    expect(primary[0]!.textContent).toContain("更新到 0.8.99 并重启");

    // 低优先级的通道**不是被删掉**，而是降级为次要按钮（否则用户无从安装）。
    const coreBtn = screen.getByRole("button", { name: /更新核心到 v26\.9\.10/ });
    const geoBtn = screen.getByRole("button", { name: /更新 geo 到 v26\.9\.10/ });
    expect(coreBtn.className, "核心降级为次要按钮").not.toContain("btn--primary");
    expect(geoBtn.className, "geo 降级为次要按钮").not.toContain("btn--primary");
  });

  it("只有核心有新版 ⇒ 主按钮是核心那颗", async () => {
    const { section } = await renderUpdate(
      snap({
        update: {
          latest_app: release("0.8.44"),
          app_update_available: false,
          check_error_app: null,
          latest_core: release("v26.9.10", true),
          core_update_available: true,
          latest_geo: null,
        },
      }),
    );

    const primary = primaries(section);
    expect(primary).toHaveLength(1);
    expect(primary[0]!.textContent).toContain("更新核心到 v26.9.10");
    // 已是最新的客户端只给回执，不给按钮（task-44），也就不抢主按钮。
    expect(screen.getByText("已是最新版本")).toBeTruthy();
  });

  it("只有 geo 有新版 ⇒ 主按钮是 geo 那颗", async () => {
    const { section } = await renderUpdate(
      snap({
        update: {
          latest_app: null,
          app_update_available: false,
          latest_core: null,
          core_update_available: null,
          latest_geo: release("v26.9.10"),
        },
      }),
    );

    const primary = primaries(section);
    expect(primary).toHaveLength(1);
    expect(primary[0]!.textContent).toContain("更新 geo 到 v26.9.10");
  });

  it("三者都没有新版 ⇒ 0 个主按钮，且**不渲染任何安装动作**", async () => {
    const { section } = await renderUpdate(
      snap({
        update: {
          latest_app: release("0.8.44"),
          app_update_available: false,
          check_error_app: null,
          latest_core: release("v26.9.9"),
          core_update_available: false,
          // ⚠️ geo 没有三态字段（后端没有 `geo_update_available`）⇒ 这里只能给
          // 「这次检查没拿到 geo 版本」。见本文件末尾的诚实清单。
          latest_geo: null,
        },
      }),
    );

    expect(primaries(section)).toHaveLength(0);
    expect(screen.queryByText(/更新核心到/)).toBeNull();
    expect(screen.queryByText(/更新 geo 到/)).toBeNull();
    expect(screen.queryByText(/更新到 .* 并重启/)).toBeNull();
    // 「没有新版」仍要有落点（静默隐藏会退化成「点了检查更新没反应」）。
    expect(screen.getByText(/核心已是最新/)).toBeTruthy();
    expect(screen.getByText("已是最新版本")).toBeTruthy();
  });

  it("「检查更新」保持次要按钮、「回退到随包版本」保持 ghost（都不抢主按钮）", async () => {
    const { section } = await renderUpdate(
      snap({
        update: {
          latest_core: release("v26.9.10", true),
          core_update_available: true,
          core_managed: true,
          core_managed_version: "26.9.9",
        },
      }),
    );

    const check = screen.getByRole("button", { name: "检查更新" });
    const revert = screen.getByRole("button", { name: "回退到随包版本" });
    expect(check.className).not.toContain("btn--primary");
    expect(revert.className).toContain("btn--ghost");
    expect(revert.className).not.toContain("btn--primary");
    expect(primaries(section)).toHaveLength(1);
  });
});

// ---------------------------------------------------------------------------
// 3) 长字符串：路径等宽 + 可复制；复制失败必须可见
// ---------------------------------------------------------------------------

describe("3 · 路径等宽 + 可复制，且复制失败有可见反馈", () => {
  it("内核路径用 `.input.mono` 显示 + CopyButton；剪贴板被拒时给 role=alert", async () => {
    setClipboard(() => Promise.reject(new Error("NotAllowedError")));
    renderAt(snap(), "set-core");
    await screen.findByRole("button", { name: "保存并重启核心" });

    const box = document.querySelector<HTMLInputElement>("#set-core input.input.mono");
    expect(box, "内核路径要用 .input.mono（等宽、可选中）显示").not.toBeNull();
    expect(box!.value).toBe("/Applications/XrayTun.app/Contents/Resources/xray");
    expect(box!.readOnly).toBe(true);

    const copy = screen.getByRole("button", { name: "复制路径" });
    fireEvent.click(copy);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent, "复制失败不许静默").toContain("复制失败");

    // 反例（同一颗按钮、同一次渲染里换个剪贴板实现）：成功时给 role=status 的确认。
    setClipboard(() => Promise.resolve());
    fireEvent.click(copy);
    expect(await screen.findByText(/已复制到剪贴板/)).toBeTruthy();
  });

  it("数据目录也用 `.input.mono` 显示 + CopyButton（第二处复制同样有反馈）", async () => {
    renderAt(snap(), "set-misc");
    await screen.findByRole("button", { name: "打开数据目录" });

    const box = document.querySelector<HTMLInputElement>("#set-misc input.input.mono");
    expect(box, "数据目录要用 .input.mono 显示").not.toBeNull();
    expect(box!.value.length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: "复制路径" })).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// 4) TUN 提示条件化
// ---------------------------------------------------------------------------

describe("4 · TUN 说明只在核心**不支持**原生 TUN 时出现", () => {
  it("`supports_native_tun === true` ⇒ 不出现任何 TUN 升级建议", async () => {
    renderAt(snap({ core: { supports_native_tun: true, min_native_tun_version: "26.1.31" } }), "set-core");
    await screen.findByRole("button", { name: "保存并重启核心" });

    expect(screen.queryByTestId("tun-support-hint"), "支持原生 TUN 就不该再劝升级").toBeNull();
    expect(screen.queryByText(/建议使用最新的/)).toBeNull();
    expect(screen.queryByText(/TUN 模式需要/)).toBeNull();
  });

  it("`supports_native_tun === false` ⇒ 出现提示，且门槛版本来自字段、不写死 `26.9.x`", async () => {
    renderAt(snap({ core: { supports_native_tun: false, min_native_tun_version: "26.1.31" } }), "set-core");
    await screen.findByRole("button", { name: "保存并重启核心" });

    const hint = await screen.findByTestId("tun-support-hint");
    expect(hint.textContent).toContain("TUN 模式需要");
    expect(hint.textContent, "门槛必须来自 min_native_tun_version").toContain("26.1.31");
    expect(hint.textContent, "不许写死某个具体小版本").not.toContain("26.9.x");
  });
});

// ---------------------------------------------------------------------------
// 5) 进度条可访问性
// ---------------------------------------------------------------------------

describe("5 · 下载进度条是 progressbar，且不编百分比", () => {
  it("有总量 ⇒ role=progressbar + valuenow/min/max + 可读的 valuetext", async () => {
    await renderUpdate(
      snap({
        update: {
          progress: { label: "客户端更新", done_bytes: 42, total_bytes: 100 },
        },
      }),
    );

    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow")).toBe("42");
    expect(bar.getAttribute("aria-valuemin")).toBe("0");
    expect(bar.getAttribute("aria-valuemax")).toBe("100");
    expect(bar.getAttribute("aria-valuetext") ?? "").toContain("42 B");
    expect(bar.getAttribute("aria-label") ?? "").toContain("客户端更新");
  });

  it("上游没报总量 ⇒ 仍是 progressbar，但**不写** aria-valuenow（不假装有百分比）", async () => {
    await renderUpdate(
      snap({
        update: { progress: { label: "核心更新", done_bytes: 2048, total_bytes: null } },
      }),
    );

    const bar = await screen.findByRole("progressbar");
    expect(bar.getAttribute("aria-valuenow"), "总量未知时不许编一个百分比").toBeNull();
    expect(bar.getAttribute("aria-valuetext") ?? "").toContain("2 KB");
  });

  it("没有下载 ⇒ 不渲染 progressbar（不给出一条永远 0% 的假进度）", async () => {
    await renderUpdate(snap({ update: { progress: null } }));
    expect(screen.queryByRole("progressbar")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// 6) 关键事实同屏可见；开发者细节才折叠
// ---------------------------------------------------------------------------

/** 必须**不展开**就能看到的四类事实（评审点名）。 */
const CORE_FACTS = ["不会改动 App 包本身", "删掉那些文件", "重新连接核心才会生效"];
const APP_FACTS = ["退出并重启 App", "先断开隧道"];

describe("6 · 关键事实与按钮同屏；只有开发者细节可折叠", () => {
  it("关键事实都在收起的 `<details>` 之外（未展开可见）", async () => {
    const { section } = await renderUpdate(snap());

    const coreFacts = within(section).getByTestId("update-core-facts");
    const appBlock = within(section).getByTestId("update-app-block");
    const appFacts = within(appBlock).getByTestId("update-app-facts");

    for (const f of CORE_FACTS) expect(coreFacts.textContent).toContain(f);
    for (const f of APP_FACTS) expect(appFacts.textContent).toContain(f);

    // 关键事实绝不在**任何**折叠层里（展开/收起都一样可见）。
    expect(closedDetailsAncestor(coreFacts)).toBeNull();
    expect(closedDetailsAncestor(appFacts)).toBeNull();
    const foldedText = [...section.querySelectorAll("details")]
      .map((d) => d.textContent ?? "")
      .join("\n");
    for (const f of [...CORE_FACTS, ...APP_FACTS]) {
      expect(foldedText, `${f} 不许藏进折叠层`).not.toContain(f);
    }

    // 「同屏」的结构判据：客户端事实与客户端按钮在**同一块**里。
    expect(within(appBlock).getByRole("button", { name: "检查客户端更新" })).toBeTruthy();
  });

  it("开发者细节（预发布策略 / 配额 / SHA256SUMS / 日志路径）必须在**默认收起**的 details 里", async () => {
    const { section } = await renderUpdate(snap());

    const coreDetails = within(section).getByTestId("update-core-details");
    expect(coreDetails.tagName).toBe("DETAILS");
    expect((coreDetails as HTMLDetailsElement).open, "默认必须收起").toBe(false);
    expect(coreDetails.textContent).toContain("预发布");

    const appDetails = within(section).getByTestId("update-app-details");
    expect(appDetails.tagName).toBe("DETAILS");
    expect((appDetails as HTMLDetailsElement).open, "默认必须收起").toBe(false);
    for (const t of ["60 次/小时", "SHA256SUMS", "app-update.log"]) {
      expect(appDetails.textContent, `${t} 属于开发者细节，应当在折叠层里`).toContain(t);
    }
  });
});

// ---------------------------------------------------------------------------
// 诚实清单（本文件测不到什么）
// ---------------------------------------------------------------------------
//
// 1. **geo 没有三态字段**：后端只有 `core_update_available` / `app_update_available`，
//    没有 `geo_update_available`（`apps/desktop/src/state.rs`、`commands/snapshot.rs` 都没有）。
//    所以「geo 查到了但已是最新」在快照里表达不出来 —— 本文件里
//    「三者都没有新版」只能把 `latest_geo` 给 `null`。真要收掉那颗白跑的按钮，
//    需要后端补一个三态字段（本卡不许改 `apps/desktop`，见报告）。
// 2. **「同屏」不是「同视口」**：jsdom 没有布局引擎，量不了像素；
//    这里断言的是「同一块 + 不在折叠里」这个结构代理。
// 3. **颜色/字号**：CSS 不加载，`.input.mono` 只断言类名挂上了；
//    「真的等宽、真的折行」由 `updateSourceGuard.test.ts` 扫 CSS 钉住。
