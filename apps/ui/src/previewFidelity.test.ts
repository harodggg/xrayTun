/**
 * 预览快照必须与**真实快照类型**同形（task-87）。
 *
 * # 为什么需要这条
 *
 * `?preview=1` 是我们做**所有视觉 / 交互 / 体量测量**的唯一依据（真机 WKWebView 无法自动化）。
 * 而 `previewSnapshot.ts` 的各个块过去靠 `as unknown as …` 通过类型检查
 * ⇒ **编译器完全帮不上忙** ⇒ 少字段、字段名过期全是**静默**的（页面照常渲染）。
 *
 * 实测到的后果（本卡修掉的那批）：
 * * `tun` 块用的是**另一套字段名**（`capture_ipv6`/`ipv6_mode`/`bypass_hosts`/…）⇒
 *   预览里「隧道网段 / 哨兵 DNS」输入**是空的**、「绕过局域网」复选框与真实默认值**相反**；
 * * `helper` 块没有 `version_check` ⇒ 预览态下「助手版本不一致」的提示**永不出现**；
 * * `runtime` 缺 `recovery`、订阅缺 `enabled`/`update_interval_hours`、
 *   `dns.probes` 的 `kind: "direct"`（真实取值只有 `domestic`/`foreign`）与 `error`（应为 `note`）。
 *
 * # 这条断言在测什么
 *
 * **真类型的字段集 ⊆ 预览提供的字段集**（缺失项必须列进 `EXEMPT` 并写明理由）。
 * 它不是「比对两个手写清单」——**字段集直接从 `types.ts` 的接口体里解析出来**，
 * 所以真类型新增字段而预览没跟上时，这里会红。
 *
 * 另一层保险在源码里：本卡把 `as unknown as` 断言**去掉了**，那些块现在由
 * `tsc` 直接约束（少字段/多字段都编译不过）。这条测试覆盖的是「有人把断言加回去」的情况。
 *
 * ⚠️ 已知边界（不假装解决）：**只有 TS 里声明过的字段才在比较范围内**。
 * 例如 `settings.auto_reconnect` 目前**不在** `AppSettings` 里（task-71 有意没改 types.ts），
 * 所以它不在本断言的射程内 —— 预览对它的处理是「缺省 ⇒ 界面按后端默认值 true 显示」。
 */
import { describe, expect, it } from "vitest";

import { scenarioSnapshot } from "./previewSnapshot";

/**
 * 显式豁免：**故意**不提供的字段（每条都要写清为什么）。
 * 当前为空 —— 也就是说预览已经覆盖了它手写的每一个块的全部真实字段。
 * 将来若要豁免，写成 `{ iface: "X", field: "y", why: "…" }`，并在这里说明理由。
 */
const EXEMPT: Array<{ iface: string; field: string; why: string }> = [];

/** 预览手写的块 → 它应当满足的真实接口。 */
const BLOCKS: Array<{ iface: string; path: string }> = [
  { iface: "AppSnapshot", path: "" },
  { iface: "AppSettings", path: "settings" },
  { iface: "TunSettings", path: "settings.tun" },
  { iface: "DnsSettings", path: "settings.dns" },
  { iface: "FakeDnsSettings", path: "settings.fakedns" },
  { iface: "CoreRuntime", path: "runtime" },
  { iface: "RecoveryState", path: "runtime.recovery" },
  { iface: "HelperAvailability", path: "helper" },
  { iface: "Node", path: "nodes[0]" },
  { iface: "Subscription", path: "subscriptions[0]" },
  { iface: "CoreAvailability", path: "core" },
  { iface: "LoginItemState", path: "login_item" },
  { iface: "TrafficSample", path: "traffic" },
  { iface: "UpdateStatus", path: "update" },
  { iface: "DnsStatus", path: "dns" },
  { iface: "DnsProbe", path: "dns.probes[0]" },
];

/** 从 `types.ts` 的源码里取出某个 interface 的**字段名**（顺带支持 `type X = {...}`）。 */
function fieldsOf(src: string, iface: string): string[] {
  const noComments = src.replace(/\/\*\*[\s\S]*?\*\//g, "").replace(/^\s*\/\/.*$/gm, "");
  const m = new RegExp(`export (?:interface|type) ${iface}\\b[^{]*\\{([\\s\\S]*?)\\n\\}`).exec(
    noComments,
  );
  if (!m) throw new Error(`types.ts 里找不到 ${iface} 的定义（改名了？）`);
  return [...m[1]!.matchAll(/^\s{2}([A-Za-z_][A-Za-z0-9_]*)\??\s*:/gm)].map((x) => x[1]!);
}

/** 按 `settings.tun` / `dns.probes[0]` 这种路径取值。 */
function at(root: unknown, path: string): unknown {
  if (!path) return root;
  return path.split(".").reduce<unknown>((acc, seg) => {
    const mm = /^(\w+)\[(\d+)\]$/.exec(seg);
    if (mm) return (acc as Record<string, unknown>)?.[mm[1]!] instanceof Array
      ? ((acc as Record<string, unknown>)[mm[1]!] as unknown[])[Number(mm[2])]
      : undefined;
    return (acc as Record<string, unknown>)?.[seg];
  }, root);
}

describe("预览快照的保真度（task-87）", () => {
  it("预览提供的字段必须覆盖真实快照类型（缺失项只能在 EXEMPT 里显式豁免）", async () => {
    // 与 topbarStatus.test.tsx 同款：本包没装 @types/node，用**非字面量**动态 import 绕开静态解析
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const src = fs.readFileSync(path.resolve("src", "types.ts"), "utf8");

    const snap = scenarioSnapshot() as unknown as Record<string, unknown>;
    const problems: string[] = [];

    // 防空壳：字段提取如果真的失效（全 0 个字段），下面的比较会**恒真**。
    // 当前精确值是 **132**（task-89 补 `auto_reconnect` 之前是 **131**）；
    // 新增字段时**同步上调**，这样「解析器失效」与「字段被误缩进」都会先在这里暴露。
    const extracted = BLOCKS.map((b) => fieldsOf(src, b.iface));
    const total = extracted.reduce((n, f) => n + f.length, 0);
    expect(total, "从 types.ts 抽出的字段总数太少 —— 这条断言基本成了空壳").toBeGreaterThan(130);
    expect(
      fieldsOf(src, "HelperAvailability"),
      "抽查：HelperAvailability 必须含 version_check（否则是解析器没跟上）",
    ).toContain("version_check");

    for (const { iface, path: p } of BLOCKS) {
      const real = fieldsOf(src, iface);
      const preview = at(snap, p);
      if (preview === undefined || preview === null) {
        problems.push(`${p || "(root)"} 在预览里不存在（期望满足 ${iface}）`);
        continue;
      }
      const have = new Set(Object.keys(preview as Record<string, unknown>));
      const exempt = new Set(EXEMPT.filter((e) => e.iface === iface).map((e) => e.field));
      const missing = real.filter((f) => !have.has(f) && !exempt.has(f));
      if (missing.length) {
        problems.push(`${p || "(root)"} (${iface}) 缺: ${missing.join(", ")}`);
      }
    }

    expect(
      problems.length,
      `预览快照与真实类型不同形 —— 预览是我们唯一的测量依据，缺字段会让测量静默失真：\n${problems.join("\n")}`,
    ).toBe(0);
  });

  it("本卡修掉的那几处确实在预览里（不是「断言为空所以通过」）", () => {
    const s = scenarioSnapshot();
    // 防「空壳」：抽查几条本次补齐的真实字段
    expect(s.settings.tun.network, "隧道网段").toBe("198.18.0.1/15");
    expect(s.settings.tun.sentinel_dns, "哨兵 DNS").toBe("198.18.0.2");
    expect(s.settings.tun.bypass_private, "绕过局域网（真实默认 true）").toBe(true);
    expect(s.settings.tun.bind_outbound_to, "出站绑定（null = 自动）").toBeNull();
    expect(s.settings.tun.ipv6, "IPv6 处理").toBe("passthrough");
    expect(s.runtime.recovery.recovering, "recovery 空闲态").toBe(false);
    expect(s.subscriptions[0]!.enabled, "订阅 enabled").toBe(true);
    expect(s.subscriptions[0]!.update_interval_hours, "订阅间隔（Rust 默认 24）").toBe(24);
    expect(s.dns.probes[0]!.kind, "探测分组取值域").toBe("domestic");
    // task-89：`auto_reconnect` 现在**声明进 TS** 了（撤掉 task-71 的类型旁路），
    // 所以它进了本断言的射程；预览值取 Rust 默认 true。
    expect(s.settings.auto_reconnect, "auto_reconnect（Rust 默认 true）").toBe(true);
    // helper 的版本核对：**明确的预览态**，不是假装成 match/mismatch
    expect(s.helper.version_check.state).toBe("unreadable");
    expect(
      s.helper.version_check.state === "unreadable" ? s.helper.version_check.reason : "",
      "reason 必须写明这是预览态",
    ).toContain("预览");
  });
});
