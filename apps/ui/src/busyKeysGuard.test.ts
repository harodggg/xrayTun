/**
 * busy 键守卫：`run("key")` 的键必须能在 `BUSY_LABEL` 里找到人话。
 *
 * # 用户看到的缺陷（P0 bug 3）
 *
 * 顶栏 busy 徽章靠 `store.tsx::busyLabelOf`。这张表**只认内部英文键**；
 * 表中没有的键 → 只说兜底句「有操作正在进行…」。而更新那一节调用点写的键
 * 与表里的键**对不上**：
 *
 * | 调用点（`Settings.tsx`） | 表里的键（`store.tsx`） |
 * |---|---|
 * | `install-core` | `install-core-update` |
 * | `install-geo` | `install-geo-update` |
 * | `revert-update` | `revert-managed-update` |
 * | `check-app` | `check-app-update` |
 * | `install-app` | `install-app-update` |
 *
 * ⇒ 五个更新操作**全部**退化成「有操作正在进行…」——用户不知道在装什么、等多久。
 *
 * # 守卫怎么工作
 *
 * 逐文件扫生产 `*.tsx`，取出 `run("…")` / `runVoid("…")` 的**字面量**第一个实参，
 * 要求每个 **ASCII kebab-case 内部键**都在 `BUSY_LABEL` 里有条目。扫描前剥掉
 * 行注释与块注释（`Dashboard.tsx` 的说明注释里也写着 `run("stop"|"start", …)`）。
 *
 * # 它测不到什么（诚实清单）
 *
 * * `Intent.tsx` 的六处 `runVoid("应用意图规则", …)` 把**中文句子**当键用 ——
 *   它们同样落进兜底句，但那是既有的另一类写法（本卡不改），守卫按
 *   「内部键 = ASCII」把它们排除，不假装覆盖；
 * * 键是**运行时拼**出来的（模板串 / 变量）—— 本守卫只认字面量。
 */
import { describe, expect, it } from "vitest";

/** 去掉行注释与块注释（`https://` 里的 `//` 不算：它前面是 `:`）。 */
function stripComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, "")
    .replace(/(^|[^:])\/\/.*$/gm, "$1");
}

/** `run("key"` / `runVoid("key"` 的第一个字面量实参（按出现顺序；可重复）。 */
export function busyKeysIn(src: string): string[] {
  const code = stripComments(src);
  const out: string[] = [];
  for (const m of code.matchAll(/\b(?:run|runVoid)\(\s*"([^"]*)"/g)) out.push(m[1]!);
  return out;
}

/** 从 `store.tsx` 源码解析 `BUSY_LABEL`（键 → 人话）。 */
export function busyLabelsIn(src: string): Map<string, string> {
  const m = /const BUSY_LABEL: Record<string, string> = \{([\s\S]*?)\n\};/.exec(src);
  if (!m) throw new Error("store.tsx 里找不到 BUSY_LABEL（改名了？）");
  const map = new Map<string, string>();
  for (const line of m[1]!.split("\n")) {
    const kv = /^\s*"?([A-Za-z][\w-]*)"?\s*:\s*"([^"]*)"/.exec(line);
    if (kv) map.set(kv[1]!, kv[2]!);
  }
  return map;
}

/** 扫描器本体：返回所有「ASCII 键没有对应人话」的 `文件: run("key")`。 */
export function unlabeledBusyKeys(
  files: Array<{ file: string; text: string }>,
  labels: Map<string, string>,
): string[] {
  const problems: string[] = [];
  for (const { file, text } of files) {
    for (const key of busyKeysIn(text)) {
      // 内部键是 ASCII kebab-case；中文句子（Intent.tsx 的既有写法）不在射程内。
      if (!/^[a-z][a-z0-9-]*$/.test(key)) continue;
      if (!labels.has(key)) problems.push(`${file}: run("${key}") 在 BUSY_LABEL 里没有条目`);
    }
  }
  return problems;
}

describe("busy 键与 BUSY_LABEL 对齐（P0 bug 3）", () => {
  it("扫描器本体：漏映射的键会被抓，中文键与注释里的调用不误报", () => {
    const labels = new Map([["probe", "正在测试延迟…"]]);
    const files = [
      {
        file: "A.tsx",
        text: [
          '  run("probe", () => x);', // 有映射
          '  run("install-core", () => x);', // 漏映射 ⇒ 必须抓
          '  runVoid("open-login-items", () => x);', // 漏映射（runVoid）⇒ 必须抓
          '  runVoid("应用意图规则", async () => {});', // 中文键 ⇒ 不报
          "  // run(\"ghost-comment\", () => x);", // 行注释 ⇒ 不报
          "  /* run(\"block-comment\", () => x); */", // 块注释 ⇒ 不报
        ].join("\n"),
      },
    ];
    expect(unlabeledBusyKeys(files, labels)).toEqual([
      'A.tsx: run("install-core") 在 BUSY_LABEL 里没有条目',
      'A.tsx: run("open-login-items") 在 BUSY_LABEL 里没有条目',
    ]);
  });

  it("五个更新操作的键名与 BUSY_LABEL **逐字**对齐（正面清单 + 旧键不得复活）", async () => {
    const fs = await import("node:" + "fs");
    const path = await import("node:" + "path");
    const store = fs.readFileSync(path.resolve("src", "store.tsx"), "utf8");
    const settings = fs.readFileSync(path.resolve("src", "pages", "Settings.tsx"), "utf8");
    const labels = busyLabelsIn(store);
    const used = new Set(busyKeysIn(settings));

    const canonical = [
      "install-core-update",
      "install-geo-update",
      "revert-managed-update",
      "check-app-update",
      "install-app-update",
    ];
    for (const key of canonical) {
      expect(labels.has(key), `BUSY_LABEL 必须定义 ${key}`).toBe(true);
      expect((labels.get(key) ?? "").length > 0, `${key} 的人话不能为空`).toBe(true);
      expect(used.has(key), `Settings.tsx 必须用 ${key} 调 run()`).toBe(true);
    }
    for (const wrong of ["install-core", "install-geo", "revert-update", "check-app", "install-app"]) {
      expect(used.has(wrong), `旧键 ${wrong} 不得再出现（否则顶栏只说兜底句）`).toBe(false);
    }
  });

  it("全仓生产 `*.tsx`：每个 ASCII busy 键都有 BUSY_LABEL 条目", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
      readdirSync: (p: string, o: { withFileTypes: true }) => Array<{
        name: string;
        isDirectory: () => boolean;
      }>;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
      join: (...parts: string[]) => string;
      relative: (from: string, to: string) => string;
    };

    const root = path.resolve("src");
    const store = fs.readFileSync(path.join(root, "store.tsx"), "utf8");
    const labels = busyLabelsIn(store);
    // 防空壳：解析失效时「没问题」没有意义。
    expect(labels.size, "BUSY_LABEL 解析出来的条目太少").toBeGreaterThan(20);
    expect(labels.has("probe"), "抽查：probe 必须在表里").toBe(true);

    const files: Array<{ file: string; text: string }> = [];
    const walk = (dir: string) => {
      for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
        if (e.name === "node_modules" || e.name === "dist") continue;
        const full = path.join(dir, e.name);
        if (e.isDirectory()) walk(full);
        else if (e.name.endsWith(".tsx") && !e.name.endsWith(".test.tsx")) {
          files.push({ file: path.relative(root, full), text: fs.readFileSync(full, "utf8") });
        }
      }
    };
    walk(root);
    expect(files.length, "扫描到的 .tsx 太少，可能是路径/遍历失效").toBeGreaterThan(15);

    const problems = unlabeledBusyKeys(files, labels);
    expect(
      problems,
      `这些 busy 键在 BUSY_LABEL 里没有条目 ⇒ 顶栏只说兜底句「有操作正在进行…」：\n  ` +
        problems.join("\n  "),
    ).toEqual([]);
  });
});
