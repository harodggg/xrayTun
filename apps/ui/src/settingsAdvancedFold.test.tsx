/**
 * 「连接」分类里的 TUN 高级参数收进折叠（task-78）。
 *
 * # 为什么做
 *
 * 「连接」是设置页的默认分类、也是用户最常打开的一类，而它此前实测 **1158px**（视口 813px）
 * —— 用户为了改「代理入口 / Fake-IP」得先滚过一屏多，其中大头是**默认值几乎总是对、
 * 改了才需要懂**的旋钮（隧道网段 / MTU / IPv6 / 出站绑定接口）。
 *
 * # 口径（**必须写清，两种含义不同**）
 *
 * 用的是**原生 `<details>`**（复用 `Routing.tsx` 既有的 `page__details` 范式）⇒
 * 收起时内容**仍在 DOM 里**，由浏览器的 UA 样式隐藏（`display: none`）——
 * **不是「不渲染」**。所以：
 * * 读屏：内容用户代理样式隐藏，不会被朗读；键盘：收起时不可聚焦（浏览器行为）；
 * * 表单：受控输入仍在 DOM，React 不会卸载它们 ⇒ **值得以保留**（这正是下面「值不丢」那条要钉的）；
 * * 测试口径：jsdom 不实现 details 的可见性计算，所以这里断言的是**容器收起**（`details.open === false`）
 *   + 内容仍在 DOM；**「真的不可见」由真浏览器实测**（`checkVisibility()` 返回 false，见报告）。
 *
 * # 这一组钉住什么
 *
 * 1. 默认收起、展开后同一批 DOM 节点仍在（没有卸载重建）；
 * 2. **反例**：判定「留明面」的项**不在任何折叠里** ⇒ 展开/收起两种状态下都可见（防一刀切）；
 * 3. **值不丢**：展开 → 改 MTU → 保存 → 收起 → 重新展开，值仍是改后的；
 * 4. 三个分节都还在（没有把整节折没了）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

/** 预览快照的 `tun` 块缺若干字段（它是 `as unknown as` 转的），这里补全成一份真实形状。 */
function fullSnap() {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: {
      ...base.settings,
      mode: "tun",
      tun: {
        ...base.settings.tun,
        mtu: 1500,
        network: "198.18.0.1/15",
        sentinel_dns: "198.18.0.2",
        ipv6: "passthrough",
        bypass_private: true,
        bind_outbound_to: null,
      },
    },
  } as never;
}

async function renderSettings() {
  mocks.snapshot.mockResolvedValue(fullSnap());
  mocks.tailLogs.mockResolvedValue([]);
  const r = render(
    <StoreProvider>
      <Settings />
    </StoreProvider>,
  );
  await screen.findByRole("tablist");
  return r;
}

const fold = () => document.querySelector<HTMLDetailsElement>("#set-tun details");
const summary = () => fold()!.querySelector("summary")!;
const mtuInput = () => fold()!.querySelector<HTMLInputElement>('input[type="number"]')!;
const toggle = () => fireEvent.click(summary());

beforeEach(() => {
  vi.clearAllMocks();
  /** 后端「原样持久化回传的整份设置」——这样读回的值才是真的来自载荷。 */
  mocks.saveSettings.mockImplementation(async (s: unknown) => ({ ...(fullSnap() as object), settings: s }));
});

describe("TUN 高级参数收进折叠（task-78）", () => {
  it("默认收起「高级」，展开后**同一批 DOM 节点**仍在（原生 details 不卸载）", async () => {
    await renderSettings();

    expect(fold(), "TUN 节里应当有一个折叠").not.toBeNull();
    expect(fold()!.open, "默认必须是收起的").toBe(false);
    // 可发现性：标签里有「高级」二字，不是只有一个看不出能点的小三角
    expect(summary().textContent, "标签必须写清「高级」").toContain("高级");
    // 可访问性：原生 details/summary ⇒ 键盘可达、aria-expanded 由浏览器正确暴露（不是自造控件）
    expect(summary().tagName).toBe("SUMMARY");

    const before = mtuInput();
    expect(before, "口径：内容仍在 DOM 里（靠 UA 样式隐藏，不是不渲染）").not.toBeNull();

    toggle();
    expect(fold()!.open).toBe(true);
    expect(mtuInput(), "展开后必须是**同一个**输入节点（没有卸载重建）").toBe(before);
  });

  it("反例：判定「留明面」的项**不在任何折叠里** ⇒ 展开/收起两种状态下都可见", async () => {
    await renderSettings();

    // 这些是刻意留在明面的：哨兵 DNS（它的说明是「隧道没了 DNS 还指着它 ⇒ 全网断」的唯一解释）、
    // bypass_private（决定家里 NAS / 局域网还能不能直连，属于网络出问题时最先要确认的项）、
    // 以及本节标题与「当前模式」上下文。
    const mustStayVisible = [
      screen.getByText(/把内网 \/ 链路本地 \/ 多播地址排除在隧道之外/),
      screen.getByText("哨兵 DNS"),
      screen.getByText(/写入系统的「假」解析器/),
      screen.getByText(/当前模式：/),
    ];
    for (const el of mustStayVisible) {
      expect(el.closest("details"), `「${el.textContent?.slice(0, 12)}…」被折进折叠里了`).toBeNull();
    }

    // 两种状态下都在（收起时已断言；展开后再确认一次，防「展开反而把它们藏了」）
    toggle();
    for (const el of mustStayVisible) {
      expect(document.contains(el), "展开后明面项不见了").toBe(true);
      expect(el.closest("details")).toBeNull();
    }
  });

  it("**值不丢**：展开 → 改 MTU → 保存 → 收起 → 重新展开，值仍是改后的", async () => {
    await renderSettings();

    toggle(); // 展开
    expect(fold()!.open).toBe(true);
    const input = mtuInput();
    expect(input.value, "初始值来自设置").toBe("1500");

    fireEvent.change(input, { target: { value: "1400" } });
    expect(input.value).toBe("1400");

    // 保存（出现「有未保存的改动」条）
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
    const payload = mocks.saveSettings.mock.calls[0]![0] as { tun: { mtu: number } };
    expect(payload.tun.mtu, "载荷里必须带着改后的 MTU").toBe(1400);

    // 收起 → 重新展开
    toggle();
    expect(fold()!.open).toBe(false);
    toggle();
    expect(fold()!.open, "重新展开").toBe(true);
    expect(mtuInput().value, "收起再展开把值丢了 —— 折叠最常踩的那个坑").toBe("1400");
  });

  it("折叠只影响 TUN 高级项：三个分节都在，且折叠里不含其它分节", async () => {
    await renderSettings();
    for (const id of ["set-entry", "set-tun", "set-fakeip"]) {
      expect(document.getElementById(id), `${id} 不见了`).not.toBeNull();
    }
    // 折叠只包住高级项：入口/Fake-IP 两节不能被卷进去
    expect(document.getElementById("set-entry")!.closest("details")).toBeNull();
    expect(document.getElementById("set-fakeip")!.closest("details")).toBeNull();
  });
});
