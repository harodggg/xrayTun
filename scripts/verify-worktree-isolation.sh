#!/usr/bin/env bash
#
# 验证「worktree 必须用自己的 CARGO_TARGET_DIR」这件事**真的在挡产物身份混淆**。
#
# 复现的是 task-110 撞到的同类假错误：**同名同版本、源码不同**的两个 crate 共用一个 target dir
# ⇒ 消费者链到旧 rlib ⇒ `error[E0425]: … not found in \`libx\``（而源码里明明有）。
#
# 用法：
#     ./scripts/verify-worktree-isolation.sh                 # 绿 = 隔离生效
#     ./scripts/verify-worktree-isolation.sh --sensitivity   # 把隔离**去掉**（两侧都用共享 target dir）
#                                                            # ⇒ 同一批断言必须变红（退出码 1）
#
# 只写 /tmp（临时 mini workspace），不碰仓库、不碰主树的 target dir、不联网。

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

SENS=0
[ "${1:-}" = "--sensitivity" ] && SENS=1
[ "${1:-}" = "--sensitivity" ] && shift || true

TMP="$(mktemp -d "${TMPDIR:-/tmp}/wt-isolation-verify.XXXXXX")"
cleanup() { rm -rf "$TMP" 2>/dev/null || true; }
trap cleanup EXIT INT TERM

SHARED="$TMP/shared-target"          # 两个 checkout 共用的 target dir（要证明它危险）
ISO="$TMP/iso-target"                # 隔离后的独立 target dir
if [ "$SENS" = "1" ]; then
  ISO="$SHARED"                      # 敏感性：**把隔离去掉**
fi

WS="$TMP/ws"
pass=0; fail=0
ok() { echo "  ✓ $*"; pass=$((pass + 1)); }
no() { echo "  ✗ $*"; fail=$((fail + 1)); }
hdr() { echo; echo "=============================================================="; echo "  $*"; echo "=============================================================="; }

echo "worktree 产物身份验证：SENS=${SENS}（共享 target dir=${SHARED}）"

# ------------------------------------------------------------------ mini workspace
hdr "[0] 造一个 mini workspace：同名同版本的两个 checkout（源码不同）"
mkdir -p "$WS/libx/src" "$WS/app/src"
cat >"$WS/Cargo.toml" <<'EOF'
[workspace]
members = ["libx", "app"]
resolver = "2"
EOF
cat >"$WS/libx/Cargo.toml" <<'EOF'
[package]
name = "libx"
version = "0.1.0"
edition = "2021"
EOF
cat >"$WS/app/Cargo.toml" <<'EOF'
[package]
name = "app"
version = "0.1.0"
edition = "2021"
[dependencies]
libx = { path = "../libx" }
EOF
# 基线：libx 只有 Foo（= 「旧 checkout / 回退版」）
printf 'pub struct Foo;\n' >"$WS/libx/src/lib.rs"
printf 'fn main() { let _ = libx::Foo; }\n' >"$WS/app/src/main.rs"
echo "  mini workspace：${WS}（libx 0.1.0 / app 0.1.0）"

run_cargo() { # $1 = target dir
  ( cd "$WS" && CARGO_HOME="${CARGO_HOME:-$ROOT/../.cargo}" CARGO_TARGET_DIR="$1" cargo build -p app 2>&1 )
}

# 用「旧源码」在共享 target dir 里建立指纹（模拟：worktree 先编过一份旧 rlib）
BASELINE_OUT="$(run_cargo "$SHARED")"; BASELINE_RC=$?
if [ "$BASELINE_RC" = "0" ]; then
  ok "基线构建成功（旧源码 → 共享 target dir 里有了 libx 的旧 artifact）"
else
  no "基线构建失败（环境问题？）"; printf '%s\n' "$BASELINE_OUT" | tail -5 | sed 's/^/       /'
fi

# 造「同名同版本、源码不同」：libx 加一个 Bar（模拟另一个 checkout 的新源码），
# 并把 libx 源文件的 mtime **恢复到基线时的值** —— 这正是 cargo 指纹被满足的条件，
# 也是 task-110 里 `touch store.rs` 就能让它重新编译的原因。
MT="$(stat -f %m "$WS/libx/src/lib.rs" 2>/dev/null || stat -c %Y "$WS/libx/src/lib.rs")"
printf 'pub struct Foo;\npub struct Bar;\n' >"$WS/libx/src/lib.rs"
if touch -t "$(date -r "$MT" '+%Y%m%d%H%M.%S')" "$WS/libx/src/lib.rs" 2>/dev/null; then :; fi
# 消费者用**新**源码（新 mtime）⇒ 它会重编，但依赖的 rmeta 是旧的
printf 'fn main() { let _ = core::mem::size_of::<libx::Bar>(); }\n' >"$WS/app/src/main.rs"
sleep 1
echo "  libx/src/lib.rs 内容已变（多了 Bar），mtime 恢复成基线值 ${MT}；app 用新源码"

# ------------------------------------------------------------------ [1] 不隔离 ⇒ 假错误
hdr "[1] 共享 target dir（**不隔离**）⇒ 必须出现「源码里明明有」的假错误"
SHARED_OUT="$(run_cargo "$SHARED")"; SHARED_RC=$?
echo "$SHARED_OUT" | tail -8 | sed 's/^/       /'
echo "       退出码 = $SHARED_RC"
case "$SHARED_OUT" in
  *"not found in \`libx\`"*|*"E0425"*|*E0412*)
    ok "复现了产物身份假错误（消费者链到旧 rlib；源码里其实有 Bar）" ;;
  *)
    if [ "$SHARED_RC" != "0" ]; then
      ok "共享 target dir 下构建失败（非 0），属于要复现的现象（错误文本未匹配已知模式）"
    else
      no "共享 target dir 下**竟然构建成功** ⇒ 没复现出该现象（本验证的前提不成立）"
    fi ;;
esac

# ------------------------------------------------------------------ [2] 隔离 ⇒ 通过
hdr "[2] 独立 target dir（**隔离**）⇒ 必须构建成功"
# ⚠️ 敏感性模式下 ISO == SHARED：**不能删** —— 删了就把要复现的 stale artifact 也删了，
# 于是「去掉隔离」反而变绿（这正是本脚本第一次跑敏感性时踩到的坑）。
if [ "$ISO" != "$SHARED" ]; then rm -rf "$ISO"; fi
ISO_OUT="$(run_cargo "$ISO")"; ISO_RC=$?
echo "$ISO_OUT" | tail -6 | sed 's/^/       /'
echo "       退出码 = $ISO_RC"
if [ "$ISO_RC" = "0" ]; then
  ok "隔离后构建成功（新源码被真的编进去）"
else
  no "隔离后仍然失败 ⇒ 隔离没生效"
fi

# ------------------------------------------------------------------ [3] 守卫
hdr "[3] 守卫：worktree + 共享 target dir ⇒ 必须警告；WT_STRICT=1 ⇒ 退出码 75"
WT_NAME="verify-iso-$$"
WT_DIR="$("$ROOT/scripts/wt.sh" path "$WT_NAME")"
"$ROOT/scripts/wt.sh" new "$WT_NAME" HEAD >/dev/null 2>&1 || no "建 worktree 失败"
if [ -d "$WT_DIR" ]; then
  GUARD_OUT="$( cd "$WT_DIR" && CARGO_TARGET_DIR="$ROOT/../.cargo-target" "$ROOT/scripts/wt.sh" check 2>&1 )"
  case "$GUARD_OUT" in
    *"共享 CARGO_TARGET_DIR"*) ok "命中「共享 target dir + cwd 在 worktree」并给出明确警告" ;;
    *) no "守卫没有报警（危险！）" ;;
  esac
  STRICT_OUT="$( cd "$WT_DIR" && CARGO_TARGET_DIR="$ROOT/../.cargo-target" WT_STRICT=1 "$ROOT/scripts/wt.sh" check 2>&1 )"
  STRICT_RC=$?
  [ "$STRICT_RC" = "75" ] && ok "WT_STRICT=1 ⇒ 退出码 75（与「代码失败」区分开）" || no "WT_STRICT=1 退出码 = ${STRICT_RC}（期望 75）"
  echo "$STRICT_OUT" | grep -q "WT_STRICT=1" && ok "失败原因写明了是共享 target dir" || no "失败原因不明确"

  # ------------------------------------------------------------------ [4] wt.sh run 自动隔离 + 走锁
  hdr "[4] \`wt.sh run\`：自动隔离 target dir，并且**同时**走构建锁"
  RUN_OUT="$("$ROOT/scripts/wt.sh" run "$WT_NAME" -- sh -c 'echo "CHILD_TARGET=$CARGO_TARGET_DIR"' 2>&1)"
  echo "$RUN_OUT" | sed 's/^/       /'
  CHILD_TARGET="$(printf '%s' "$RUN_OUT" | sed -n 's/^CHILD_TARGET=//p' | head -1)"
  if [ -n "$CHILD_TARGET" ] && [ "$CHILD_TARGET" != "$SENS" ]; then :; fi
  case "$CHILD_TARGET" in
    *".cargo-target.wt/"*) ok "子进程拿到的是独立 target dir：$CHILD_TARGET" ;;
    *) no "子进程的 CARGO_TARGET_DIR 不是独立目录：${CHILD_TARGET:-（空）}" ;;
  esac
  case "$RUN_OUT" in
    *"已获取构建锁"*) ok "同一次运行确实走了构建锁（时序与身份两层都在）" ;;
    *) no "没有看到构建锁的获取行" ;;
  esac
  "$ROOT/scripts/wt.sh" rm "$WT_NAME" >/dev/null 2>&1
else
  no "worktree 没建起来，[3][4] 无法验证"
fi

hdr "结果"
echo "  pass=$pass fail=$fail"
if [ "$fail" = "0" ]; then
  echo "  ✓ 产物身份验证通过：不隔离会出假错误、隔离后正常、守卫会警告、run 同时走锁"
  exit 0
fi
echo "  ✗ 产物身份验证失败（见上面每一条 ✗）"
[ "$SENS" = "1" ] && echo "      （这是 --sensitivity 模式：把隔离去掉后**必须**出现这些红，红 = 验证真的在验隔离）"
exit 1
