#!/usr/bin/env bash
# ---------------------------------------------------------------------------
# 发行版 `XRAYTUN_TEAM_ID` 注入的**单一来源**：哨兵值、判据、归一化。
#
# 被谁用：
#   · `scripts/package-macos.sh`（本机/CI 打真正的发行包）
#   · `.github/workflows/release.yml` 的预检步骤（写进 `$GITHUB_ENV`，后续所有步骤都有值）
#   · `scripts/verify-team-id-injection.sh`（证据/断言）
#
# # 为什么需要它（安全审计 P0-1 的 F1）
#
# `crates/xt-helper/src/peer.rs` 的 `PeerPolicy::from_build_env()` 用
# `option_env!("XRAYTUN_TEAM_ID")` 决定授权策略：
#   · 注入且非空 → `RequireSignature{anchor apple generic and identifier "com.xraytun.desktop"
#     and certificate leaf[subject.OU] = "<team>"}`（只有我们签名的 App 能连上）；
#   · 未注入 / **空串** → `InsecureAllowAny`（只靠 socket `root:admin 0660`，等于"该用户能跑的
#     任何进程都能让 helper 以 root 装任意自签 CA"）。
#
# 而 release.yml 从来没有注入过它 ⇒ 发行版的第二道门是空的。本文件把"注入"变成
# **发版流程里一道显式判据**，缺了就走不到打包。
#
# # 判据（**唯一**，别再在别处写第二份）
#
#   分类 `XRAYTUN_TEAM_ID`：
#     · `real`     —— 合法 Apple Team ID：恰好 10 位 `[A-Z0-9]`。用它 ⇒ 门真正起作用。
#     · `sentinel` —— 哨兵 `UNSET-REFUSE-PRIVILEGED-OPS`。它**故意不是**合法 Team ID
#                     （含 `-`、长度 >10），所以要求串永远匹配不上任何签名 ⇒
#                     helper **拒绝一切非 root 特权操作**（fail closed）。
#                     这是"没有 Developer ID 证书时"唯一诚实的形态：宁可拒绝服务，
#                     也不"信任任何对端"。
#     · `empty`    —— 未设或空串 ⇒ **失败**（见下）。
#     · `invalid`  —— 其它形状 ⇒ **失败**（十有八九是拼错了，不能放行）。
#
#   `empty` 为什么必须**失败**而不是"自动用哨兵"：
#     · `XRAYTUN_TEAM_ID=` 显式空串会**盖掉**任何默认值，而 `peer.rs` 的判据是
#       `Some(team) if !team.is_empty()` ⇒ 空串直接退化成 `InsecureAllowAny`（实机复核：
#       `XRAYTUN_TEAM_ID= cargo test -p xt-helper build_env_policy_never_silently_trusts_everything
#        -- --nocapture` 会打印"当前构建未注入 XRAYTUN_TEAM_ID"，即宽松分支被走到）。
#       所以"空"是一个**会静默开门的值**，必须当成显式错误。
#     · GitHub Actions 里 `${{ vars.X }}` 未定义时展开成**空串**（不是"未设置"），
#       所以这条判据正是 CI 上最容易踩的形态。
#     · 出错信息里给出两条明确出路（真实 Team ID / 显式哨兵），让人**有意识地**选，
#       而不是让流水线替他默认。
#
#   ⚠️ 为什么不用 `.cargo/config.toml` 的 `[env]` 做默认值（试过，行不通）：
#     · `/.cargo` 在 `.gitignore:3` 里 ⇒ 该文件无法被 `git commit --only` 提交
#       （实测 `error: pathspec '.cargo/config.toml' did not match any file(s) known to git`），
#       要提交只能 `git add -f` 或改 `.gitignore`，两者都不该由本任务顺手做；
#     · 即便放进去，`force = false` 时**显式空串仍然胜出**（实测 `option_env = Some("")`），
#       挡不住上面那个"空串开门"的形态。
#     真正的根治要么是注进 peer.rs 的编译期判据（`crates/**`，属 backend-2 范围），
#     要么是把 `.cargo/config.toml` 变成可提交的文件 —— 两者都需要 Lead 决定。
# ---------------------------------------------------------------------------

# shellcheck shell=bash

# 哨兵：**不是**合法 Team ID（10 位 [A-Z0-9]），要求串永远匹配不上 ⇒ 一律拒绝（fail closed）。
XRAYTUN_TEAM_ID_SENTINEL='UNSET-REFUSE-PRIVILEGED-OPS'

# 合法 Apple Team ID 的形状。Apple 签发的是 10 位大写字母数字。
# ⚠️ 判据用 bash 的 `=~`，**不要**写成 `printf … | grep -Eq`：本文件会被 `set -o pipefail`
#    的脚本 source，而 `grep -q` 命中即退出 ⇒ 上游拿到 SIGPIPE（141）⇒ pipefail 把"命中"
#    判成"失败"。实测后果：产物断言给出**假红**（"找不到注入值"，其实找到了）。
#    凡"用管道把输出喂给 grep -q"都属这一类，见 verify-team-id-injection.sh 的同类注释。
XRAYTUN_TEAM_ID_REAL_RE='^[A-Z0-9]{10}$'

# team_id_classify <value> → real | sentinel | empty | invalid
team_id_classify() {
  local v="${1-}"
  if [ -z "$v" ]; then
    printf 'empty\n'
  elif [ "$v" = "$XRAYTUN_TEAM_ID_SENTINEL" ]; then
    printf 'sentinel\n'
  elif [[ "$v" =~ $XRAYTUN_TEAM_ID_REAL_RE ]]; then
    printf 'real\n'
  else
    printf 'invalid\n'
  fi
}

# team_id_fail_message —— 缺注入 / 形状不对时的可读出路（打印到 stderr）。
team_id_fail_message() {
  cat >&2 <<'EOF'
✗ 发行包必须显式注入 XRAYTUN_TEAM_ID（编译期选项，决定 helper 的授权策略）。

  不注入的后果是 helper 退化成 InsecureAllowAny（只靠 socket 权限位）——
  该用户能跑的任何进程都能让 helper 以 root 装任意自签 CA，这是安全审计的 P0-1。

  两条出路，二选一（**没有默认值**，因为两条路的后果完全不同）：

  A. 真实 Team ID（门真正起作用）：
       在 GitHub 仓库 Settings → Variables 里设 XRAYTUN_TEAM_ID=<10 位大写字母数字>，
       或本机 `XRAYTUN_TEAM_ID=ABCDE12345 ./scripts/package-macos.sh`。
       注意：需要 **Developer ID 证书** 才能签出带该 Team ID 的 App，
       而本仓库当前没有（ad-hoc 签名，`security find-identity` 为 0 个身份）。

  B. 显式哨兵（fail closed：helper 拒绝一切非 root 特权操作）：
       XRAYTUN_TEAM_ID=UNSET-REFUSE-PRIVILEGED-OPS ./scripts/package-macos.sh
       或把仓库变量设成同一个字符串。
       后果要知情：TUN 模式 / 安装 helper / MITM 信任锚**全部不可用**，
       但绝不会"信任任何对端"。这比留一个静默的门要安全。
EOF
}

# team_id_resolve [--into <file>]
#   读 `XRAYTUN_TEAM_ID`，判据通过就打印 `XRAYTUN_TEAM_ID=<值>` 一行（可写进 $GITHUB_ENV），
#   并把人话打到 stderr；不通过则返回 1（调用方必须停下）。
#   `--into -`（默认）就是 stdout。
team_id_resolve() {
  local dest='-'
  while [ $# -gt 0 ]; do
    case "$1" in
      --into) dest="${2:--}"; shift 2 ;;
      *) echo "team_id_resolve: 未知参数 $1" >&2; return 2 ;;
    esac
  done

  local value="${XRAYTUN_TEAM_ID-}"
  local cls
  cls="$(team_id_classify "$value")"
  case "$cls" in
    real)
      echo "[team-id] ✓ 注入真实 Team ID（${value}）⇒ helper 只接受 anchor apple generic +" \
           "com.xraytun.desktop + OU=${value} 的 App" >&2
      ;;
    sentinel)
      cat >&2 <<EOF
[team-id] ⚠️  **fail-closed 形态**：注入的是哨兵（${XRAYTUN_TEAM_ID_SENTINEL}），不是真实 Team ID。
[team-id] ⚠️  这个包里的 helper 会**拒绝一切非 root 特权操作**：
[team-id] ⚠️  点「安装 helper」/ TUN 模式 / MITM 信任锚都不会工作。
[team-id] ⚠️  这是有意的：宁可拒绝服务，也不退化成「信任任何对端」（P0-1）。
[team-id] ⚠️  要让门真正起作用，需要 Developer ID 证书 + 真实 Team ID（见 scripts/team-id.sh）。
EOF
      ;;
    empty)
      echo "✗ XRAYTUN_TEAM_ID 未设置或为空串 —— 空串会让 peer.rs 的判据（Some(team) 且非空）" >&2
      echo "  直接退化成 InsecureAllowAny，所以这里当**错误**处理，不猜默认值。" >&2
      team_id_fail_message
      return 1
      ;;
    *)
      echo "✗ XRAYTUN_TEAM_ID 形状不合法：'${value}'" >&2
      echo "  期望：10 位 [A-Z0-9]（Apple Team ID），或显式哨兵 ${XRAYTUN_TEAM_ID_SENTINEL}。" >&2
      team_id_fail_message
      return 1
      ;;
  esac

  if [ "$dest" = '-' ]; then
    printf 'XRAYTUN_TEAM_ID=%s\n' "$value"
  else
    printf 'XRAYTUN_TEAM_ID=%s\n' "$value" >>"$dest"
  fi
  return 0
}
