# worktree 必须用自己的 `CARGO_TARGET_DIR`（产物身份，不是并发时序）

> 一句话：**同一个 `CARGO_TARGET_DIR` + 多个 checkout = 同名同版本 crate 的产物身份混淆。**
> 症状是「源码里明明有的类型却报 `E0425`」，或者更糟：**敏感性实验静默链到对面那份，得出假结论**。
> 规矩：worktree 里跑构建用 `./scripts/wt.sh run <name> -- <命令>`（自动隔离 + 自动拿锁）。

## 1. 实例原文（task-110 实测，2026-09-22）

```
error[E0425]: cannot find type `TailLogStats` in module `xt_core::store`   ← 源码里明明有
```

排查与坐实（backend-dev 给的证据链）：

* `/tmp/wt-lock109` 那个 worktree 停在 `6144d3c`（**早于** `d0b5e39`），其
  `crates/xt-core/src/store.rs` 里 `tail_logs_with_stats` 出现 **0 次**；
* 那次构建用的却是**主工作区的** `CARGO_TARGET_DIR=/Users/xbtg-/deepseek-harness/.cargo-target`
  （从 `pgrep` 的完整命令行看到）；
* ⇒ desktop 构建**链到了那个 worktree 编出来的旧 `xt-core` rlib**；
* **坐实方式**：`touch crates/xt-core/src/store.rs` 强制重编主树那份 ⇒ **立刻编译通过**。

## 2. 为什么这不是并发问题（所以 `task-109` 的锁不解决它）

| | `scripts/build-lock.sh`（task-109） | 本卡（task-112） |
|---|---|---|
| 管什么 | **并发时序**：谁先谁后 | **产物身份**：链到哪一份 |
| 失效场景 | 两个进程同时写同一个 target dir | 一个进程，但 checkout 与 target dir 不匹配 |
| 后果 | 假红（`E0463 can't find crate`） | 假红（`E0425`）**或**假绿（敏感性实验“回退也不红”） |

**排队排完照样可能链错**：同一个 target dir 里装着两个 checkout 的同名同版本 crate
（`xt-core 0.8.34` vs `xt-core 0.8.34`，源码不同），cargo 的指纹可能互相满足 ⇒ 谁最后写就链谁。

**两者能同时工作，而且天然不冲突**：锁是**按 target dir** 放的（`${CARGO_TARGET_DIR}.lock.d`）——
worktree 用自己的 target dir ⇒ 也用自己的锁，连排队都不会跟主树抢。
`./scripts/wt.sh run` 一次把两件事都做了：**独立 target dir + 走构建锁**。

## 3. 机制：`scripts/wt.sh`

```bash
./scripts/wt.sh new fix1 6144d3c          # 建 worktree（顺带软链 node_modules 与 binaries，不复制大文件）
./scripts/wt.sh run fix1 -- cargo test -p xt-core --lib
                                          # ★ 在 fix1 里跑：CARGO_TARGET_DIR=<repo>/../.cargo-target.wt/fix1
                                          #   并且先拿构建锁（时序 + 身份两层都在）
./scripts/wt.sh env fix1                  # 打印 export（想自己 cd 进去时用）
./scripts/wt.sh path fix1                 # 打印 worktree 路径
./scripts/wt.sh check [目录]              # 守卫：共享 target dir + cwd 在 worktree ⇒ 警告（WT_STRICT=1 ⇒ 75）
./scripts/wt.sh list                      # 列出每个 worktree 与它**应该**用的 target dir
./scripts/wt.sh rm fix1                   # 删 worktree，同时删它自己的 target dir
```

`check.sh` 里也内置了同一条守卫（**三态**）：**只有两侧都成功取到真实路径才比较** ——
两侧相同 ⇒ 共享警告（`WT_STRICT=1` ⇒ 75）；不同 ⇒ 不吭声；**任一取不到 ⇒ 说「无法判定」**
（写明「这是环境问题，不是代码失败」，普通模式继续跑、`WT_STRICT=1` ⇒ 75）。
**已知摩擦（不是 bug）**：全新 checkout 上主 target dir 还不存在 ⇒ `WT_STRICT=1` 会给一次 75，
先跑一次构建即可。验证脚本：`scripts/verify-worktree-guard.sh`（T1–T5 + 双向敏感性）。
（主工作区的 target dir 是从 `git rev-parse --git-common-dir` 推出来的 ——
在 worktree 里用 `$ROOT/../.cargo-target` 比会**漏判**，第一版就漏了。）

## 4. 复现与双向敏感性（`scripts/verify-worktree-isolation.sh`）

造一个 mini workspace：`libx 0.1.0`（先只有 `Foo`，再改成 `Foo + Bar`）与 `app 0.1.0`
（消费者改成用 `Bar`），并把 `libx` 源文件的 mtime **恢复成基线值** ——
这正是 cargo 指纹被满足的条件，也是 `touch` 就能“治好”它的原因。

```
$ ./scripts/verify-worktree-isolation.sh
  [1] 共享 target dir（**不隔离**）⇒
       error[E0425]: cannot find type `Bar` in crate `libx`
        --> app/src/main.rs:1:48
       1 | fn main() { let _ = core::mem::size_of::<libx::Bar>(); }
       error: could not compile `app` (bin "app") due to 1 previous error
       退出码 = 101
  ✓ 复现了产物身份假错误（消费者链到旧 rlib；源码里其实有 Bar）
  [2] 独立 target dir（**隔离**）⇒ Compiling libx / Compiling app / Finished，退出码 = 0
  ✓ 隔离后构建成功（新源码被真的编进去）
  [3] 守卫：worktree + 共享 target dir ⇒ 警告；WT_STRICT=1 ⇒ 退出码 75
  [4] `wt.sh run`：子进程拿到独立 target dir，且同一次运行走了构建锁
  pass=8 fail=0   ✓ 产物身份验证通过

$ ./scripts/verify-worktree-isolation.sh --sensitivity     # 把隔离去掉（两侧都用共享 target dir）
  ✗ 隔离后仍然失败 ⇒ 隔离没生效        （同一份断言在无隔离时必然红）
  pass=7 fail=1   ✗ 产物身份验证失败（退出码 1）
```

> 敏感性模式第一次跑时**没有变红**：脚本在跑隔离侧之前 `rm -rf` 了那个 target dir，
> 把要复现的 stale artifact 一起删了 —— **测试自身制造了「假绿」**。现在只在两侧目录不同时才删。

## 5. worktree 放哪里：**不能放在 `${TMPDIR}` 下**（task-185，2026-09-24 实测事故）

本卡上面说的是「target dir 不能共用」；这一节说的是**worktree 自己的目录放在哪**——同类错误的另一半：
**看起来稳的东西其实建在会被系统清理的地方。**

### 5.1 事故原文（2026-09-24 16:25，Lead 复核）

`scripts/wt.sh` 当时的默认值是 `WT_DIR_ROOT="${TMPDIR:-/tmp}/xraytun-wt"`，而本机
`TMPDIR=/var/folders/68/…/T/` 是 **macOS 的可清理临时目录**。系统清掉整棵树之后：

```
$ git worktree list
…/T/xraytun-wt/leadgate182   91f3d7a  (detached HEAD)  prunable
…/T/xraytun-wt/t183          7deedb9  (detached HEAD)  prunable
…/T/xraytun-wt/v179          af582bf  (detached HEAD)  prunable
```

`wt.sh path` 报 `No such file or directory`；其中 `t183` 是 backend-dev **正在编译**的 worktree，
`v179` 是 tester **正在做突变验证**的 ⇒ **两个队友的在途工作被静默打断**，`leadgate182` 那次门禁也白跑。
**target dir 没丢**（`.cargo-target.wt/{leadgate182,t183,v179}` 共 6.0G 仍在）⇒ 重建后可复用。
失败方式是「跑到一半目录没了」，**很容易被读成「测试自己挂了」**。

### 5.2 改了什么

| 项 | 改前 | 改后 |
|---|---|---|
| 默认 `WT_DIR_ROOT` | `${TMPDIR:-/tmp}/xraytun-wt`（本机 = `/var/folders/…/T/`） | **`<主工作区>/../.wt`** = `/Users/xbtg-/deepseek-harness/.wt`（与 `WT_TARGET_ROOT` 对称，不会被系统清理；§5.4 起锚到**主工作区**而不是当前 checkout） |
| 落在 `${TMPDIR}` 之下 | 静默按旧行为继续 | **大声警告**（说明会被清理、给出建议），`WT_STRICT=1` ⇒ **退出 75** |
| 能看「会放哪」 | 无 | `./scripts/wt.sh dir`：打印解析后的绝对路径 + `under_tmpdir=yes/no`；`new` 也会打印 |
| 路径解析 | `norm()`（目录不存在时原样返回 ⇒ `/var` 与 `/private/var` 前缀判不出来） | 新增 `norm_nonexist()`：把仍存在的祖先 `pwd -P` 解析后再拼回剩余部分 |

实测（真实输出，节选自 `wt.sh new` / `run` / `rm` 一次完整走一遍）：

```
$ env -u WT_DIR_ROOT ./scripts/wt.sh dir
  · WT_DIR_ROOT = /Users/xbtg-/deepseek-harness/.wt （不在临时目录下 ✓）
WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt
under_tmpdir=no

$ WT_DIR_ROOT="$TMPDIR/xraytun-wt-probe" ./scripts/wt.sh dir      # 退出码 0（只警告）
  ⚠️  **worktree 会建在系统的临时目录里，可能被清掉**：
      WT_DIR_ROOT = /private/var/folders/…/T/xraytun-wt-probe
      TMPDIR      = /private/var/folders/…/T   （macOS 的可清理临时目录）
  …（写明 2026-09-24 16:25 的事故与建议）…
under_tmpdir=yes

$ WT_STRICT=1 WT_DIR_ROOT="$TMPDIR/xraytun-wt-probe" ./scripts/wt.sh dir
  ✗ WT_STRICT=1：worktree 在临时目录下 ⇒ 明确失败（退出码 75）        # 退出码 = 75

$ ./scripts/wt.sh new wt185probe HEAD
  · WT_DIR_ROOT = /Users/xbtg-/deepseek-harness/.wt （不在临时目录下 ✓）
  ✓ worktree: /Users/xbtg-/deepseek-harness/.wt/wt185probe （ref=4a937d2）
  ✓ 它的 target dir（**独立**）: /Users/xbtg-/deepseek-harness/.cargo-target.wt/wt185probe

$ ./scripts/wt.sh run wt185probe -- true
  ▶ 在 /Users/xbtg-/deepseek-harness/.wt/wt185probe 运行（CARGO_TARGET_DIR=…/wt185probe ← 独立；并走构建锁）
  🔒 已获取构建锁… 🔓 已释放构建锁：pid=5060 持有 1s                    # 退出码 0
$ ./scripts/wt.sh rm wt185probe                                       # 只删我自己的，别人的条目一个没动
```

### 5.3 自测（可复跑，`docs/verification/verify-wt-dir-root.sh`）

```
$ bash docs/verification/verify-wt-dir-root.sh
  口径：本脚本所在 checkout = … ；主工作区 = …
[1] 不设 WT_DIR_ROOT ⇒ 不在临时目录下、且 = <主工作区>/../.wt        ✓✓
[2] 显式 = $TMPDIR/… ⇒ 出现警告 + under_tmpdir=yes + 退出码 0        ✓✓✓
[3] WT_STRICT=1 + 临时目录 ⇒ 退出 75                                ✓✓
[4] 反向敏感性：把守卫从副本里去掉 ⇒ 警告消失（案子 [2] 的断言变红）   ✓
[5] 边界：恰好 = $TMPDIR ⇒ yes ；同级前缀相近（…/T2/wt）⇒ no          ✓✓
[6] 嵌套 worktree：worktree 里解析到与主树**同一处**、无 `.wt/.wt`，
    且 `WT_ANCHOR=checkout`（旧行为）⇒ 真的产生 `.wt/.wt`（断言会红）  ✓✓✓✓✓
== 汇总：pass=16 fail=0 ==
```

**已接进 `scripts/check.sh`**（`step "worktree 位置守卫自测（wt.sh：默认不在 ${TMPDIR} 下）"`）——
判据不再靠「记得手动跑」。注意：**门禁总是在 linked worktree 里跑 `check.sh`**，
所以案子 [6]（锚定主工作区）是这条自测真正的价值所在。

### 5.4 锚在**主工作区**，不是当前 checkout（task-187，`task-185` 的补丁遗漏）

`task-185` 把默认值改成 `$ROOT/../.wt`，但 `$ROOT` 是**当前 checkout**。从 worktree 内部运行时：

```
$ cd <主树>          && bash docs/verification/verify-wt-dir-root.sh
  ✓ …（WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt）                     ← 正确
$ cd .wt/leadgate182 && bash docs/verification/verify-wt-dir-root.sh
  ✓ 解析结果不在临时目录下（WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt/.wt）  ← 嵌套，而且**还是绿的**
```

两条真实代价（不是外观问题）：

1. **target dir 复用失效**：在 worktree 里 `wt.sh new x` ⇒ worktree 落 `.wt/.wt/x`、target 落
   `.wt/.cargo-target.wt/x` ⇒ **不命中**既有 `.cargo-target.wt/<名>` ⇒ 每个都全量重编（~2 GB / 数分钟），
   而这套 per-worktree target dir 的设计本来就是为了避免它；
2. **同源假绿**：案子 [1] 的期望值当时也用同一个 `$ROOT` 算 ⇒ 主树与 worktree **都绿**、含义却不同。

**改法**：默认值（`WT_DIR_ROOT` / `WT_TARGET_ROOT` / `MAIN_TARGET`）改用脚本里早就有的
`main_worktree()`（读 `git rev-parse --git-common-dir` 的父目录）**锚到主工作区**；
`CARGO_HOME` / `npm_config_cache` / `node_modules`、`binaries` 的软链源也同样锚到 `MAIN_ROOT`
（否则 worktree 里会各配一份 `.wt/.cargo`，把 registry 重下一遍）。显式传入的环境变量**保持原样**。
测试缝 `WT_ANCHOR=checkout` 用来还原旧行为，供反向敏感性使用。

实测（主树 / worktree 两处原始输出一致）：

```
$ ./scripts/wt.sh dir                     # 主树
WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt
WT_TARGET_ROOT=/Users/xbtg-/deepseek-harness/.cargo-target.wt
$ cd .wt/wt187env && ./scripts/wt.sh dir  # worktree（脚本是同一份修好的副本）
WT_DIR_ROOT=/Users/xbtg-/deepseek-harness/.wt
WT_TARGET_ROOT=/Users/xbtg-/deepseek-harness/.cargo-target.wt
$ diff <(./scripts/wt.sh env probe) <(cd .wt/wt187env && ./scripts/wt.sh env probe)   → 空（逐行相同）
```

⇒ 改后 `wt.sh new <名>` 在**两个位置**都会命中**同一个** `.cargo-target.wt/<名>`。
现有 `.wt/.wt` **不存在**（那次只打印了解析结果，没有真的建目录）⇒ 无需迁移，也没有 `prune` 别人的条目。

**在 worktree 里跑整条自测**（门禁的真实场景；把修好的两份文件拷进一个临时 worktree）：

```
$ cd .wt/wt187gate && bash docs/verification/verify-wt-dir-root.sh
  口径：本脚本所在 checkout = …/.wt/wt187gate ；主工作区 = …/xray-tun
[6] ✓ 主树/ worktree 的 WT_DIR_ROOT 与 WT_TARGET_ROOT 逐行相同、无 `.wt/.wt`
    ✓ 反向敏感性：WT_ANCHOR=checkout ⇒ /Users/xbtg-/deepseek-harness/.wt/.wt（≠ 主树）
== 汇总：pass=16 fail=0 ==
```

## 6. 诚实清单：本卡**不**覆盖的场景

* **同一个 target dir 里混着旧 release 产物**（`target/release` 与 `target/debug` 的交叉、
  或手工拷进去的二进制）——那既不是 worktree 也不是并发；
* **cargo 版本差异**：不同 cargo 的指纹语义可能不同；本卡的实验都在同一个 toolchain 上跑；
* **同一次 checkout 内**、用 `cp -p`/`tar` 保留 mtime 的方式“换源码”⇒ 可能不重编（本卡用这个机制做复现，
  它同样是**真实风险**，但成因不是 worktree）；
* **`target` 目录被外部工具改写**（清缓存脚本、rsync 等）；
* `wt.sh` 的 node_modules / binaries 是**软链回主树**的 ⇒ 不要在 worktree 里改它们
  （它们是主树的文件）；要改就在主树改，或在 worktree 里删掉软链重建。

### 6.1 §5 的「位置守卫」覆盖不到什么

* **显式把 worktree 放进 `${TMPDIR}` 仍然只是警告**（默认不失败）——那可能是有意的临时实验；
  要硬拦就用 `WT_STRICT=1`。守卫的作用是让你**不会不知道**，不是禁止；
* **已经建在旧位置的 worktree 不会被自动迁移**：`git worktree list` 里显示 `prunable` 时需人工重建
  （`git worktree remove --force <目录>` + `git worktree prune` + `wt.sh new …`）——
  **不要替别人 prune** 可能正在使用的条目；
* 守卫只看 **`WT_DIR_ROOT`**：若把 `WT_TARGET_ROOT` 显式指到临时目录，本守卫**不管**
  （target dir 丢了不会中断在途工作，只是重编一遍，代价不同）；
* 「会被系统清理」是 **macOS 对 `/var/folders/…/T` 的行为**；其它平台上 `/tmp` 不一定被清 ——
  守卫判的是「在 `${TMPDIR}` 之下」，**不是**「一定会被清」；
* 自测只覆盖 `wt.sh` 的路径解析与守卫（不建 worktree、不跑 cargo）⇒ 它证明默认值与守卫行为，
  **不**证明「新建的 worktree 一定能编译」；后者由 §5.2 里那次真实的 `new` + `run -- true` 作证。

### 6.2 §5.4 的「锚到主工作区」覆盖不到什么

* `WT_ANCHOR=checkout` 是**测试缝**：谁在 CI 里设它就等于把锚定退回旧行为（只允许出现在自测脚本里）；
* 锚定靠 `git rev-parse --git-common-dir`：**不在 git 仓库里**（例如解包出来的目录）时会退回 `$ROOT`
  —— 那时 `wt.sh` 本来也用不了（`worktree add` 需要仓库）；
* **已经建在 `.wt/.wt/<名>` 的 worktree 不会被自动迁移**（本次 `.wt/.wt` 并未被建出来，所以没这个问题）；
  真出现了就人工 `git worktree remove --force` + 重建，**不要 `prune`** 别人的条目；
* 自测案子 [6] 会**临时建一个探针 worktree**（`<主工作区>/../.wt/wt187probe`）并在结束时删掉；
  它在 `trap … EXIT` 里清理 ⇒ 若进程被 `kill -9` 可能残留一个空 worktree 条目，需人工 `git worktree prune`。
