/**
 * geo 编号重复来源守卫（P0 bug 2）。
 *
 * # 缺陷
 *
 * `geo_tag` 由后端 `crates/xt-core/src/update.rs::install_geo` 落盘，写作
 * `format!("{base} {}", available.version)`；而同文件 `geo_urls` 返回的 `base`
 * **就是** `available.version`（`tag.clone()`）⇒ 落盘的是同一个编号印两遍
 * （`v26.9.9 v26.9.9`）。前端 `Settings.tsx` 把 `snapshot.update.geo_tag`
 * **原样**上屏，于是用户在「geo 数据」那一行看到重复串。
 *
 * # 本仓库的边界
 *
 * 本次任务不允许改 `crates/**`，所以重复在**后端快照层**
 * （`apps/desktop/src/commands/snapshot.rs`）的**两个消费点**收敛
 * （快照字段 + 「geo 数据已更新到 …」那条日志），**不是**在前端去重。
 *
 * # 为什么这条守卫在 `apps/ui`
 *
 * 这条缺陷对前端是**不可见**的（它只收到一个已经拼好的字符串）⇒ 前端 DOM 断言
 * 无论如何都会在改前就绿。本机的门禁只有 vitest（Rust 不在本地编译），所以把
 * 「后端快照层必须走 `geo_tag_for_display`、不许把裸 `meta.geo_tag.clone()` 交给
 * 界面/日志」这条契约放在 vitest 里**扫源码**钉住 —— 改前它红，改后它绿。
 */
import { describe, expect, it } from "vitest";

describe("geo_tag 的重复来源已在后端快照层收敛（P0 bug 2）", () => {
  it("snapshot.rs：geo_tag 只经 `geo_tag_for_display` 到达界面与日志，裸 clone 不得存在", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };

    // vitest 的 cwd 是 apps/ui（与 jsxTextGuard / previewFidelity 同款）。
    const snapshotRs = path.resolve("..", "desktop", "src", "commands", "snapshot.rs");
    const src = fs.readFileSync(snapshotRs, "utf8");
    // 只看生产源码：测试模块里的 fixture 字符串不参与判据。
    const prod = src.split("\n#[cfg(test)]\nmod tests")[0] ?? src;

    expect(prod, "必须存在收敛函数 geo_tag_for_display").toContain("fn geo_tag_for_display(");
    const uses = (prod.match(/geo_tag_for_display\(/g) ?? []).length;
    expect(
      uses,
      "至少 3 处：函数定义 + 两个消费点（快照字段 / 更新日志）",
    ).toBeGreaterThanOrEqual(3);
    expect(
      prod.includes(".geo_tag.clone()"),
      "裸 `meta.geo_tag.clone()` 会把重复串直接交给界面/日志 —— 必须改走 geo_tag_for_display",
    ).toBe(false);
  });

  it("前端只做透传：`Settings.tsx` 不去重、也不自己再拼一遍 geo 编号", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
    };
    const settings = fs.readFileSync(path.resolve("src", "pages", "Settings.tsx"), "utf8");
    expect(settings).toContain("snapshot.update.geo_tag");
    // 「修掉重复来源」= 不在前端去重 ⇒ 展示点附近不许出现 split/Set 之类的去重动作。
    const row = settings.slice(
      settings.indexOf("geo 数据"),
      settings.indexOf("geo 数据") + 220,
    );
    expect(row, "前端不该对 geo_tag 做 split/dedupe").not.toMatch(/split\(|new Set\(/);
  });
});
