# task-147：把「版本号替换」与「Release Notes 的用户动作」从纪律变成机制

> 由来：v0.8.35 发布期间暴露两处**靠纪律、不靠机制**的地方 ——
> ① 版本号替换脚本只活在 `/tmp`；② Release 正文里的「本版需要**重新**安装特权助手」
> 不在 CI 模板里（实测 `v0.8.34` 的 body 逐字就是静态模板），只能发布后手工 `gh release edit` 补。

## 1. 交付物

| 文件 | 作用 |
|---|---|
| `scripts/bump-release.py`（新） | 三个子命令：`phase1`（已发布 → 正在发布）、`phase2`（正在发布 → 已发布）、`notes`（合成 Release 正文）；外加 `self-test` |
| `docs/release-notes/v0.8.35.md`（新） | **本版用户动作的单一来源**（v0.8.35 的「重新安装特权助手」）—— 本卡**回溯补上**，让已发布的这一版也符合新约定 |
| `.github/workflows/release.yml` | 「发布到 GitHub Release」一步不再内嵌静态 heredoc，改为 `python3 scripts/bump-release.py notes --tag "$TAG" --out NOTES.md`（模板搬进脚本，= 58 行 → 15 行） |
| `docs/verification/verify-release-notes-step.sh`（新） | 那条机制在**本地**的复刻：从 YAML 抽真身 + 假 `gh` 演练 4 种情形 |
| `infra/incident-collector/README.md` | 顺手留档 `task-123` 那条运维知识：**鉴权头只有 `X-Auth-Token`**，`Authorization: Bearer …` 一律 401 |

## 2. A：`scripts/bump-release.py`

### 2.1 三条性质（每条对应一次真实事故）

1. **先全部校验，任一条不符 ⇒ 整体退出、磁盘零改动**
   （v0.8.33 出过「页面写 `0.8.33` 文件名 + `0.8.32` 字节数」的中间态，根因是管道吞掉退出码）；
2. **顺序模拟校验**：「已发布 → 正在发布」那组 pattern 依赖前面的版本号替换**先生效**，
   拿原始文本校验会全 0 命中（本工具第一版就踩过，被干跑抓住）⇒ 在内存里按序应用，
   每一步都在**上一步的结果**上数命中次数，全过之后才写盘；
3. **旧 og 卡片与引用切换同一个提交**：v0.8.35 的线上 `og:image` 404 窗口就是这两件事被拆开造成的
   ⇒ `phase1` 自己删旧卡片，**删之前**断言 `site/**` 里对它的引用为 0（引用数按**模拟后的**文本算，
   否则干跑会假红 —— 第一版就假红过一次）。

### 2.2 覆盖范围（8 处 + `Cargo.lock`，与卡面逐条对齐）

`Cargo.toml` / `apps/desktop/tauri.conf.json` / `apps/ui/package.json` /
`gen-site-{jsonld,geo,images}.py` / `site/assets/site.js` / `site/{,en/}index.html` 的下载文件名 +
`Cargo.lock` 的 5 个 workspace 成员；外加站点过渡文案与字节数清空、生成器 `PUBLISHED` 开关。

### 2.3 真仓库演练（`--dry-run`，时点 2026-09-23）

```
$ python3 scripts/bump-release.py phase1 --new 0.8.36 --date 2026-09-24 --dry-run
[phase1 干跑] 0.8.35 → 0.8.36（仓库 /Users/xbtg-/deepseek-harness/xray-tun）
  ✓ Cargo.toml                       [workspace.package] version 命中 1（预期 1）
  …（43 条 ✓，逐条 printed）…
✓ 全部 42 条校验通过（顺序模拟）；干跑，不落盘。将写 12 个文件：
    Cargo.toml / apps/desktop/tauri.conf.json / apps/ui/package.json /
    scripts/gen-site-{jsonld,geo,images}.py / site/assets/site.js /
    site/wasm/index.html / site/en/wasm/index.html / Cargo.lock /
    site/index.html / site/en/index.html
[旧 og 卡片] site/** 里对 0.8.35 的引用 = 0（必须 0）
  （干跑）会删 site/og-image-0.8.35.png
  （干跑）会删 site/og-image-en-0.8.35.png
```
（`✓` 行 43 条：42 条规则 + 1 条「引用 = 0」。）

**干跑零落盘（两级证据）**：①`git status --porcelain` 在这 12 个「将写」路径上为空；
②把六个交付文件（含本工具自己）的 sha256 汇总成一条指纹，干跑前后**相同**：

```
before=3d302f63df6ec48fdb4a940c797543b4667607f1e4c0dee1c866214e557bfea5
after =3d302f63df6ec48fdb4a940c797543b4667607f1e4c0dee1c866214e557bfea5
```
（汇总口径：`shasum -a 256 <6 个文件> | shasum -a 256`。）

### 2.4 自测（`self-test`）：绿 / 红 / **零落盘**

夹具 = `git archive 442d65d`（v0.8.34 的「提交 2」= **真实已发布态**）+ `git init` 提交一次，
所以每一步都能用 `git status --porcelain` 取证。默认夹具 `--fixture-ref 442d65d` 可覆盖。

```
$ python3 scripts/bump-release.py self-test
[self-test] 夹具 = `442d65d` 的树（版本 0.8.34，已发布态）→ 目标 0.8.35
--- T1 绿：phase1 干跑 + 真跑 ---
  ✓ T1a 干跑：退出 0 且 `git status --porcelain` **空**（干跑不写盘）
  T1b 真跑退出码=0；git status 变更文件数=14
  ✓ T1b 旧 og 卡片 site/og-image-0.8.34.png 已在同一批里删除
--- T5 绿：phase2（真实字节数 → 已发布态）---
  ✓ T5 PUBLISHED 打开 / ✓ 真实 dmg 字节 / ✓ MiB 四舍五入为 45.2
  ✓ T5 页面上出现真实字节数 / ✓ 「正在发布」已消失 / ✓ pinned 直链已出现
--- T2 红：把 caption 的预期次数改成错的 ---
  ✓ T2 退出非 0（1）且 `git status --porcelain` 与失败前**逐字相同**（before='' after=''）
--- T3 红：拿**原始文本**校验依赖项（= 破坏顺序模拟）---
  ✓ T3 退出非 0（1）：依赖前面替换的 pattern 在原始文本上命中不足，被抓住了
--- T4 notes：缺动作文件 / 有动作 / 显式声明无动作 ---
  ✓ T4a 缺 docs/release-notes/v0.8.35.md ⇒ 退出非 0（1）
  ✓ T4b 动作段在最前、固定模板在后（2959 字节）
  ✓ T4c 显式声明「本版无需用户额外动作」也允许（但必须写出来）
[self-test] 结论：全部通过
```

* **T2 的突变点**：把 `N_CAPTION_ZH = 1` 改成 `= 2`（脚本内是**命名常量**，为的就是能这样打）；
  突变后的副本放在夹具**外面**、用 `--repo` 指向夹具 ⇒ 失败前后 `git status --porcelain`
  都是**空字符串**，这就是「失败时零落盘」的原始证据。
* **T3 的突变点**：把校验那一行的 `r.count(state[f])` 改成 `r.count(orig[f])`
  ⇒ 依赖版本号替换的 pattern 在原始文本上命中不足 ⇒ 非 0。
  （两次突变都是**改脚本副本**，不是给生产代码留开关。）
* T5 用夹具把 `phase2` 的 28 条规则**真的跑过一遍**（否则它们要等到下次发版才第一次执行），
  并在已发布态上重跑一次确认它会失败。

## 3. B：Release Notes 的版本特有动作

### 3.1 单一来源：选 `docs/release-notes/v<版本>.md`，不选 CHANGELOG 固定小节

理由（两条都是可检验的）：

1. **路径由 tag 直接派生**（`v0.8.36` → `docs/release-notes/v0.8.36.md`），判据就是 `test -f`；
   而「从 CHANGELOG 里切出某个固定二级标题的小节」要新增一套 Markdown 结构解析 ——
   标题层级、先后顺序、别的版本里同名标题都会变成新的脆弱点（本项目对「写一条永远不生效的解析」
   有过多次教训）；
2. **它必须显式存在**：忘了写 = 发版流程直接失败，而不是「悄悄少一段」。

### 3.2 判据（缺了报什么）

`scripts/bump-release.py notes` 的判定与行为：

| 情形 | 结果 |
|---|---|
| `docs/release-notes/<tag>.md` 不存在 | **非 0**，报错点名期望路径，并说明「确实没有动作也要显式写一句」 |
| 文件为空 / 只有空白 | **非 0** |
| 既没有 Markdown 标题（`## …`，允许写成引用块 `> ## …`）也没有「本版无需用户额外动作」这句 | **非 0**（无法判定是有意还是漏写） |
| 有标题 | 通过，标注「有必须的用户动作」，动作段放在正文最前 |
| 只有「本版无需用户额外动作」这一句 | 通过，标注「明确声明无需额外动作」 |

合成形状：`动作段` + `\n\n---\n\n` + `固定模板`（模板逐字来自原 `NOTES.md` heredoc，现住在脚本里）。

### 3.3 机制在本地演练：`docs/verification/verify-release-notes-step.sh`

它**从 YAML 里抽出那一步的 `run:` 真身**（PyYAML，非手抄），并断言它确实调用
`scripts/bump-release.py notes`（有人把接线改掉 ⇒ 这里红），然后用**假 `gh`**（记录调用、不发网络请求）
在一个临时目录里真跑那一段：

```
$ docs/verification/verify-release-notes-step.sh
  抽出 3598 字节的 run 真身（含 `bump-release.py notes` ✓）
  ✓ 从 release.yml 抽出该步真身（不是手抄）
  ✓ A：缺动作文件 ⇒ 非 0 退出，且报错点名了期望路径
  ✓ A：**在调用任何 gh 之前**就失败（不会先建 Release 再报错）
  ✓ B：动作段在最前、固定模板「## 安装」在后（NOTES.md 3027 字节）
  ✓ B：gh release create 用的就是 NOTES.md（8 次 gh 调用）
  ✓ C：显式声明「本版无需用户额外动作」也放行（但必须写出来）
  ✓ D：空白动作文件 ⇒ 非 0 退出（不许当「没动作」）
pass=7 fail=0
```
其中 B/C 就是卡面要求的「在**一个假版本**（`v0.8.36`）上演练」。

### 3.4 这条机制复现了 v0.8.35 真实发布过的那份正文（字节级）

v0.8.35 的 body 是当天**手工** `gh release edit` 补的；现在用机制合成 `v0.8.35` 的 NOTES：

```
$ python3 scripts/bump-release.py notes --tag v0.8.35 --out /tmp/recomposed-v0835.md
[notes] 动作来源 docs/release-notes/v0.8.35.md（1041 字节，有必须的用户动作）
[notes] 写出 /tmp/recomposed-v0835.md（3937 字节）

机制合成 NOTES.md          = 3937 字节
我当天写进 GitHub 的 body   = 3938 字节   ⇒ NOTES.md + "\n" == 当天 body ？ **True**
CI 原始 body（未补动作）    = 2890 字节   ⇒ 模板段 2889 + "\n" == CI body ？ **True**
```
⇒ ①模板从 YAML 搬进脚本**没有搬运失真**；②下次发版用机制合成的正文与我手工补的那份**字节等同**。

## 4. 顺手留档（`task-123` 的运维知识）

`infra/incident-collector/README.md` 接口表下加了一条：
端点**只读 `X-Auth-Token`**，`Authorization: Bearer <token>` **一律 401**，
且**与完全没带令牌同一响应** ⇒ 拿到 401 先看头名，别当成「令牌不对」。
（端点源码 `worker.mjs` 只取 `request.headers.get('X-Auth-Token')`。）

## 5. 诚实清单（这条机制**覆盖不到**的）

* **已经发布的正文改不了历史**：v0.8.35 的 body 仍是当天手工补的（本卡不改它）。
  机制保证的是**下一版起**不再需要手工 —— 已经手工补过的那一版不会因此变得「可复算」；
* **`release.yml` 只在推 tag 时执行** ⇒ 本卡改动**在下一个 tag 之前没有被 CI 真实执行过**。
  本地复刻（§3.3）用**假 `gh`**、只覆盖到「资产齐全」判据之前的调用序列；
  真实运行环境（ubuntu-latest + 真 `GITHUB_TOKEN` + 真网络重试）**未在本环境验证**；
* **模板搬家的风险只对 v0.8.35 这一次被证伪**：字节对照成立，不等于以后改模板时不会漂移
  （模板现在只有一处，改它就等于改所有版本的正文 —— 这是有意的，但要知情）；
* `self-test` **依赖仓里存在夹具提交 `442d65d`**（浅克隆 / 缺该提交会失败，用 `--fixture-ref` 指别的）；
* `phase2` 的 28 条规则**只在夹具上跑过**（§2.4 T5），真实仓库上只有 `phase1` 被演练过
  （当前仓库已是已发布态）；
* `--run-generators` 会调用 `python3` 与 `/usr/local/bin/python3.10`（Pillow）——
  解释器缺失时它会**非 0 停下**（不吞），但本卡没有在缺 Pillow 的环境里演练这条路径；
* 本卡**不改** `apps/**`、`crates/**`，也没有在生产机上做任何网络/路由操作。
