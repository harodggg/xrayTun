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

## 5. 诚实清单：本卡**不**覆盖的场景

* **同一个 target dir 里混着旧 release 产物**（`target/release` 与 `target/debug` 的交叉、
  或手工拷进去的二进制）——那既不是 worktree 也不是并发；
* **cargo 版本差异**：不同 cargo 的指纹语义可能不同；本卡的实验都在同一个 toolchain 上跑；
* **同一次 checkout 内**、用 `cp -p`/`tar` 保留 mtime 的方式“换源码”⇒ 可能不重编（本卡用这个机制做复现，
  它同样是**真实风险**，但成因不是 worktree）；
* **`target` 目录被外部工具改写**（清缓存脚本、rsync 等）；
* `wt.sh` 的 node_modules / binaries 是**软链回主树**的 ⇒ 不要在 worktree 里改它们
  （它们是主树的文件）；要改就在主树改，或在 worktree 里删掉软链重建。
