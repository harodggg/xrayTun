# 生成器不再抹掉非本产品条目（task-180）

> 一句话：`scripts/gen-site-geo.py` 从「只认识本产品 4 页、重写整份产物」改成
> **「本产品条目自己生成 / 外部条目按归属或标记区间保留 / 保不住就非零拒绝」**。

## 1. 为什么（真实事故，发生了两次）

* `site/jev-x-filter/**`、`site/en/jev-x-filter/**` 是**另一条工作流**的项目页；
* 它们的 `sitemap.xml` / `llms.txt` / `llms-full.txt` 收录是**手写**加进去的；
* 而 `gen-site-geo.py` 的页面清单只有本产品 4 页（`/`、`/en/`、`/wasm/`、`/en/wasm/`）
  ⇒ **每次重跑都把外部收录静默删掉**。v0.8.38 停发期间真的发生：
  提交 1（`c37eb90`）抹过一次，对方在 `7e445ca`、`fc8a471` 自己补了两次；
  随后他们在 `.github/workflows/{pages.yml,cloudflare-pages.yml}` 加了「发现入口门禁」
  （`grep -q 'jev-x-filter/'`）——那是**外部的兜底**，不该由别人的 CI 替我们发现。

**这是「机制缺口被我们自己的流程掩盖」**：只要有人跑生成器就会重演，靠每次手工合并不是机制。

## 2. 机制（三层，全部**不含任何项目名字面量**）

| 层 | 判据 | 作用 |
|---|---|---|
| **归属保留** | `sitemap.xml`：`<loc>` 不是本产品路径的 `<url>` 块逐字插回<br>`llms.txt`：`## ` 标题不在生成集里的整节逐字插回原位<br>`llms-full.txt`：**引用站内非本产品路径的索引行**逐字插回索引表 | 第三个、第四个外部项目**自动生效**，零人工合并 |
| **标记区间** | `llms-full.txt` 里 `<!-- BEGIN external:<名> --> … <!-- END external:<名> -->`：区间内容逐字插回固定锚点；索引计数行按「本产品页 + 保留下来的外部索引行」计算 | 生成文本**内部**的手写内容（头部提示、尾部分节）也有归属 |
| **大声失败** | ① 旧里有、新里没有、且带**站内非本产品路径**的整行；② 连标题一起消失、且正文提到非本产品地址的外部 `## ` 整节 | **拒绝静默删除**：非零退出 + `文件:行号` |

外加 `check` 模式：**不写盘**，把盘上产物与生成器重算结果逐字节比较（漂移即红）。

测试缝：`GEN_SITE_EXTERNAL_OFF=1` **关掉整套机制（含守卫）** ⇒ 那才是"旧行为"，
只给反向敏感性测试用（`verify-gen-site-external.sh` 的案子 [1]）。

## 3. 证据（`docs/verification/verify-gen-site-external.sh`，**全部在隔离副本里跑**）

```
pass=14 fail=0

[1] 改前对照 GEN_SITE_EXTERNAL_OFF=1（旧行为）：
    ✓ 照常退出 0（不报错）
    ✓ sitemap.xml：外部条目被抹掉 16 行      ← 复现事故
    ✓ llms.txt：外部条目被抹掉 7 行
    ✓ llms-full.txt：外部条目被抹掉 3 行
[2] 改后：跑全部生成器（gen-site-jsonld.py gen + gen-site-geo.py，rc 都是 0）
    ✓ 三份产物：Jev 与 beauty-meter 的外部条目**逐行不变**
[3] ✓ 连跑两次：三份产物零 diff（幂等）
[4] 大声失败
    ✓ 生成区块内的外部行 ⇒ 非零退出，报错指名 `site/llms-full.txt:927`
    ✓ 生成区块内的外部**整节** ⇒ 非零退出，指名 `site/llms-full.txt:929`
[5] 通用性
    ✓ `grep -nE 'jev|beauty' scripts/gen-site-geo.py`：逻辑里 **0 处**字面量（只有注释）
    ✓ 夹具 jev-x-filter：sitemap=8 / llms.txt=4（都在）
    ✓ 夹具 beauty-meter：sitemap=8 / llms.txt=4（都在）   ← **第三个外部项目**，真夹具
```

**复核命令**：

```bash
bash docs/verification/verify-gen-site-external.sh          # 五案，14/0
python3 scripts/gen-site-geo.py check                       # 只读：产物 == 生成器重算（当前 4/4 ✓）
python3 scripts/gen-site-geo.py                             # 写模式；外部条目会被保留
```

## 4. 门禁接线（**随「标记迁移」那一次提交一起进**）

`scripts/check.sh` 里加一步 `python3 scripts/gen-site-geo.py check`（与既有的
`gen-site-jsonld.py check` 同类，**只读、不写盘、秒级**）。为什么接到 `check.sh` 而不是
改 Pages 工作流：`.github/workflows/**` 不在本卡写入范围，而 `check.sh` 已经是被 CI 与发版前
共用的门禁，**下一任维护者不读本文档也会被它拦住**。

> ⚠️ 时序：这次提交**先只交机制代码**，`check.sh` 接线与 `site/llms-full.txt` 的
> **2 行标记注释**一起、在另一条工作流的 beauty-meter 提交落盘之后再交 ——
> 否则 main 上还没有标记区间，`check` 会立刻变红（而这正是它该有的行为）。

## 5. 覆盖不到的（诚实清单）

* **第三个外部项目若只改 `llms-full.txt`**，必须自己包一次标记区间（`sitemap.xml` / `llms.txt`
  按归属自动保留）——这条已写进 `llms.txt` 里给对方看的「维护提示」；
* **在生成器区块内手写**的内容会被重写：`check` 模式会红、`gen` 模式会被守卫拦下（若带站内路径），
  但**不带任何 URL 的散句**既不属于任何一节、也不带路径 ⇒ 仍会被重写（没有标记就没有归属）；
* 生成器**自己**的锚点行若被大改（`EXT_REGIONS` 里的两行），机制会**大声失败**而不是乱插 ——
  需要同步更新常量；
* `GEN_SITE_EXTERNAL_OFF=1` 是**测试缝**：谁在 CI 里设它就等于关掉保护（只允许出现在测试脚本里）；
* 本机制只覆盖 `gen-site-geo.py` 的三份产物；`gen-site-jsonld.py`（页面清单驱动）与
  `gen-site-images.py` 的外部页面处理**不在此机制内** —— 外部页面要进 JSON-LD 得登记进它的 `PAGES`；
* **没有**在真树跑过「生成器全跑一遍 ⇒ `site/**` 零 diff」：真树当时有另一条工作流的未提交内容。
  替代证据 = 隔离副本里的案子 [2]/[3] + 真树的只读 `check`（4/4 一致）。

## 6. 时点与口径

* 夹具 `beauty-meter` 取自 **2026-09-24 16:1x 另一条工作流的未提交工作树**（他们的提交尚未落盘）；
  本机制对它的保留是**按归属判定**的，不含项目名字面量；
* `[1]` 的「抹掉 16 / 7 / 3 行」是**旧行为**在**当前内容**上的实测（`GEN_SITE_EXTERNAL_OFF=1`），
  不是历史日志里的数字；
* 本页所有数字都可用上面的复核命令在**隔离副本**里复算。
