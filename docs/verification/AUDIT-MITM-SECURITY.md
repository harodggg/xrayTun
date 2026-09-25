# AUDIT-MITM-SECURITY —— P4「本地根证书 + MITM 通道」独立安全审计

审计基线：`main` @ `0e09214`（`v0.8.39 提交 1/2`）。审计人：security-reviewer（**未改任何产品代码**，本文件是本次唯一新增文件）。
审计对象：`crates/xt-tun/src/macos/trust.rs`、`crates/xt-helper/src/server.rs`（`install_trust_anchor`/`remove_trust_anchor` + 连接/授权）、`crates/xt-proto/src/lib.rs`（`InstallTrustAnchor`/`RemoveTrustAnchor`/`PROTOCOL_VERSION`）、`crates/xt-mitm/**`、`apps/desktop/src/mitm.rs`、`apps/desktop/src/commands/mitm.rs`，以及与本通道直接相关的 `apps/desktop/src/commands/core.rs`、`apps/desktop/src/commands/intent.rs`、`crates/xt-core/src/xray/config.rs`。

---

## 结论（先说最严重的）

**本通道当前没有任何在生产发行版里生效的"请求来源"校验，而且信任锚的回滚在真实崩溃/重启路径上会漏掉；这两条叠加的后果是：用户级同机进程可以让系统永久信任一张任意根证书（F1+F2），而"证书没被信任时引导规则绝不下发给核心"这条唯一 fail-open 承诺在热加路径上被绕过（F3）。**

最严重的 3 条（每条都有 `文件:行` 与可复算命令，详见对应小节）：

1. **F1（P0）helper 的第二道门是空的**：发行构建里 `XRAYTUN_TEAM_ID` 从未注入 → `PeerPolicy::from_build_env()` 直接退化成 `InsecureAllowAny`（只靠 `root:admin 0660` 的 socket 权限位，即"用户能跑的任何进程"）；即便注入了，`audit_token()` 取的 socket 选项常量也写错了（`0x005 = LOCAL_PEEREUUID`，正确是 `0x006 = LOCAL_PEERTOKEN`），实机测得前者只回 16 字节、后者回 32 字节 → 真正的签名校验路径（`SecCodeCopyGuestWithAttributes`）在签名构建里会**一律拒绝**。见 `crates/xt-helper/src/peer.rs:95,147-167,191-206`。
2. **F2（P0）信任锚会永久残留**：helper 在会话 `Up`（非 `BringingUp`）状态下崩溃/被 `kill -9`/机器重启后，`restore_stale()` 被 `is_stale()` 挡住不清理（`crates/xt-tun/src/macos/controller.rs:343-358`），桌面端的"遗留会话"判据（`stale_session || tun_active`）也漏掉这种状态（`apps/desktop/src/lib.rs:337`），随后一次 `TunUp` 会用 `SessionSnapshot::new` 直接覆盖旧快照（`crates/xt-tun/src/macos/controller.rs:70-71`）→ 旧根证书留在系统钥匙串里、且**没有记录、GUI 再也删不掉**。
3. **F3（P1）fail-open 闸门只保护"核心启动"这一条路**：`core_settings` 只在 `apps/desktop/src/commands/core.rs:191-194` 被调用；`intent_apply` 热加规则时用**原始** `settings` 重新构造整份规则表（`apps/desktop/src/commands/intent.rs:157-168`），会把 `mitm-steer` 推给一个没有 `mitm-out` 出站（或代理根本没在跑）的核心。同理，"卸载根证书后核心仍带 steer 规则 + 代理已被停"会把 opt-in 域名打到死端口。

---

## 0. 审计方法与可复算环境

- 全部结论分两类标注：**【代码可判】** = 只读代码即可判定；**【运行时已验】** = 本次实际跑过命令。
- **行号口径**：本报告所有 `文件:行` 都锚定 **`0e09214` 的已提交内容**。审计期间工作树里有其它 task 的未提交改动（`crates/xt-mitm/src/proxy.rs`、`crates/xt-core/src/xray/config.rs`、`crates/xt-intent/src/gateway.rs` 等），直接按行号 `sed` 可能错位。复算请用：

  ```bash
  git show 0e09214:<路径> | sed -n '<N>,<M>p'
  # 例：git show 0e09214:crates/xt-mitm/src/proxy.rs | sed -n '573,592p'
  ```

- 本次实际执行的命令与输出都写在各小节里；汇总见 [附录 A](#附录-a本次实际执行的复算命令)。
- 单元测试（本次实跑，跑的是**当时的工作树**；`CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo`，否则沙箱拒写 `~/.cargo`）：
  - `cargo test -p xt-mitm` → `35 passed; 0 failed`（lib）+ `7 passed`（集成）。
  - `cargo test -p xt-tun --lib` → `87 passed; 0 failed; 1 ignored`（ignored 的那条就是需要 root 的 `real_install_and_remove_round_trip`）。
  - `cargo test -p xraytun-desktop --lib mitm` → `6 passed; 0 failed`。
  - **这些测试全绿不改变下面任何一条结论**：F1/F2/F3 都落在测试没有覆盖的路径上（下面每条会指出"为什么现有测试抓不到"）。

---

## 1. 根证书私钥：落盘 / 进 git / 进日志

### F10（P3）私钥目前只活在内存，但仓库没有"防呆"网

**【运行时已验】**（本次实际执行，输出按原样）：

```bash
# 1) xt-mitm 里有没有任何文件写入？（没有 → 私钥不可能被它落盘）
grep -rn "fs::\|File::create\|OpenOptions" crates/xt-mitm/src/*.rs
# 输出：（空）exit=1
#    交叉验证：所有 write_all 都作用在 socket/TLS 上
grep -rn "write_all" crates/xt-mitm/src/*.rs
# 输出：proxy.rs:302,314,416,455,457,605,617（tls/upstream/socks 套接字），无文件写

# 2) 全仓有没有 key/pem 类文件？
find . -path ./target -prune -o -path ./apps/ui/node_modules -prune -o \
     \( -name "*.pem" -o -name "*.key" -o -name "*.p12" \) -print
# 输出：（空）

# 3) git 有没有跟踪任何 key/pem？
git ls-files | grep -Ei '\.(pem|key|p12|pfx)$' | wc -l
# 输出：0

# 4) .gitignore 会不会挡住将来误落的私钥？
git check-ignore -v weird.pem weird.key; echo "exit=$?"
# 输出：exit=1   ← **没有匹配任何 ignore 规则**
```

结论（【代码可判】+【运行时已验】）：

- `xt-mitm` **不写文件**：`LocalCa` 的 `key: KeyPair` 只用于 `signed_by`（`crates/xt-mitm/src/tls.rs:126-140`），`LocalCa` 没有 `Serialize` 实现，`Debug` 只打印 PEM 长度（`crates/xt-mitm/src/tls.rs:145-150`）。所以"CA 私钥落盘"目前在代码上不可达。
- 全仓**没有任何** `*.pem/*.key/*.p12` 文件被跟踪，也没有 key 材料写入代码：`grep -rn "serialize_pem\|serialize_der\|key_pem\|pkcs8" apps crates` 命中的只有 `crates/xt-mitm/src/tls.rs:140`（内存里把叶子私钥交给 rustls）与测试。
- **但 `.gitignore` 不忽略 `*.pem` / `*.key`**（上条 `exit=1`）。当前没有写入路径，所以这条是"未来回归会直接进 git"的风险，不是现网泄漏；按题目要求给出 grep 证据而不是"没问题"。
- 日志面：`trust.rs` 只在幂等删除时打 `fingerprint`（`crates/xt-tun/src/macos/trust.rs:242`），`commands/mitm.rs:67-75` 只打指纹，`crates/xt-mitm/**` 里没有任何 `tracing` 打印 `pem`/`key`/`cert`（命令：`grep -rn "tracing::" crates/xt-mitm/src apps/desktop/src/mitm.rs | grep -i "pem\|key\|cert"` → 空）。`xt-proto` 在反序列化失败时会打 **payload 前 120 字节**（`crates/xt-proto/src/transport.rs:83-91`）：`InstallTrustAnchor` 的 payload 开头是 `-----BEGIN CERTIFICATE-----`，**不含私钥**（helper 侧 `validate_pem` 还会拒收含 `PRIVATE KEY` 的 PEM），所以这条不是私钥泄漏面；但它是"任何一条消息解析失败都会把明文片段写进日志"的既有面，值得知道。

### F11（P3）`validate_pem` 只是子串检查，且 `install()` 失败会在系统目录留下 PEM

- `crates/xt-tun/src/macos/trust.rs:95-109`：只要求含 `BEGIN/END CERTIFICATE`、且不含 `PRIVATE KEY` 子串；对证书数量、格式、扩展、生效期**不做任何检查**。也就是说 helper 接受调用方给的**任意字节**（只要夹了一对 CERTIFICATE 标记），并把它写进 root 所有、且位于 root 执行白名单目录内的 `/Library/Application Support/XrayTun/ca/`。
- `crates/xt-tun/src/macos/trust.rs:203-216`：先写 `<指纹>.pem`，再调 `security add-trusted-cert`；后者失败时**直接返回错误，不删刚写的文件**。与模块文档"校验 → 写文件 → 才动钥匙串…… 而不是'一半做完了'"（`trust.rs:191-192`）不完全一致：文件会留下（`/Library/Application Support/XrayTun/ca` 在 helper 的**可执行白名单**里，见 `crates/xt-helper/src/server.rs:43-46`，见 [F12](#f12p3可执行白名单目录与普通用户可控内容的写入目录重叠)）。
- 复算：`cargo test -p xt-tun --lib trust` → `a_pem_with_a_private_key_or_without_a_cert_block_is_refused`、`a_bad_pem_fails_before_touching_the_filesystem` 通过；这两条只钉住了"空/含私钥/只有头没有尾"，钉不住"多张证书/非法证书"。

### F12（P3）可执行白名单目录与"调用方可控内容"的写入目录重叠

`ALLOWED_EXEC_ROOTS = ["/Library/PrivilegedHelperTools", "/Library/Application Support/XrayTun"]`（`crates/xt-helper/src/server.rs:43-46`）是 root 执行白名单；信任锚的落盘目录 `DEFAULT_CA_DIR = /Library/Application Support/XrayTun/ca` 落在其中（`crates/xt-tun/src/macos/trust.rs:41`）。写进去的文件名被指纹校验限制为 `<大写hex>.pem`（`trust.rs:115-133,203`），所以**当前**无法覆盖白名单目录里的可执行文件名；但"root 执行白名单目录"同时是一个"接受调用方字节（受 `validate_pem` 子串约束）的写入目录"这一事实本身，是下一步收紧时最容易踩的地方——`validate_pem` 对内容不做证书级校验（F11）。

---

## 2. helper（root）的攻击面

### F1（P0）发行构建里 `PeerPolicy` = `InsecureAllowAny`；代码签名这条门在生产不存在

```bash
# 1) 全仓谁引用了编译期 Team ID？
grep -rn "XRAYTUN_TEAM_ID" .   # （grep 工具，尊重 .gitignore）
# 命中：crates/xt-helper/src/peer.rs:192,201,375 与 docs/06-helper-protocol.md:50-59
# 没有任何 scripts/、.github/、build.rs、.cargo/config.toml 设置它

# 2) 发行流程注入的 env 只有这些：
grep -n "env:\|XRAYTUN" .github/workflows/release.yml | head
# 输出：只有 CARGO_TERM_COLOR / CARGO_HOME / CARGO_TARGET_DIR / npm_config_cache，
#      打包步骤的 env 只有 XRAYTUN_TARGET: universal-apple-darwin
# 3) cargo 的 [env] 注入面：
ls -a .cargo            # → 不存在
ls -a ../.cargo         # → 只有 registry/bin，无 config.toml（脚本用的 $ROOT/../.cargo）
cat apps/desktop/build.rs   # → 只有 tauri_build::build()
#    release.yml:28 的 CARGO_HOME=${{ github.workspace }}/.cargo 也是空的
```

`PeerPolicy::from_build_env()`（`crates/xt-helper/src/peer.rs:191-206`）：`option_env!("XRAYTUN_TEAM_ID")` 为 `None` 时返回 `InsecureAllowAny`；`authorize()`（`peer.rs:215-252`）对 `InsecureAllowAny` 只打一条 `warn` 就放行。于是生产上**唯一的门前置**是 socket 权限位：`restrict_socket_permissions` 设为 `root:admin 0660`（`crates/xt-helper/src/server.rs:172-193`），而 macOS 首个用户默认就在 `admin` 组 → **该用户运行的任意进程（含被诱导运行的脚本）都能连上提权 socket**。

这条对信任锚的直接影响链（全部是普通请求，没有任何额外校验）：

```
连接(通过 socket 权限) → Hello(协议号对即可，server.rs:261-272) → TunUp(建会话，server.rs:494-515)
   → InstallTrustAnchor{pem: 攻击者的自签 CA, fingerprint: 其 SHA-1}
   → trust::install() 以 root 把该 PEM 写盘 → security add-trusted-cert -d -r trustRoot -k System.keychain
```

`install_trust_anchor` 对 `pem`/`fingerprint` 的校验只有 `validate_pem`/`validate_fingerprint` 两条形状检查（`crates/xt-helper/src/server.rs:401`，实现见 `crates/xt-tun/src/macos/trust.rs:95-133`），**没有任何"这段 PEM 必须是本 App 生成的"的判据**。结果是"用户级进程 → 系统级信任根"。

### F1-b（P0）就算把 Team ID 注进去，签名校验也因为 socket 选项常量写错而必然失败

`crates/xt-helper/src/peer.rs:94-95` 手工定义：

```rust
const SOL_LOCAL: libc::c_int = 0;
const LOCAL_PEERTOKEN: libc::c_int = 0x005;   // ← 错
```

本机 SDK 与 libc crate 都写着 `0x006`，而 `0x005` 是 `LOCAL_PEEREUUID`：

```bash
SDK=$(xcrun --show-sdk-path); sed -n '85,95p' "$SDK/usr/include/sys/un.h"
# #define LOCAL_PEEREUUID  0x005  /* retrieve eff. peer UUID */
# #define LOCAL_PEERTOKEN  0x006  /* retrieve peer audit token */
grep -rn "LOCAL_PEERTOKEN\|LOCAL_PEEREUUID" /Users/xbtg-/deepseek-harness/.cargo/registry/src/*/libc-0.2.189/src/unix/bsd/apple/mod.rs
# LOCAL_PEEREUUID: c_int = 0x005;  LOCAL_PEERTOKEN: c_int = 0x006;
```

**【运行时已验】**实机对比两个选项的返回长度：

```bash
python3 -c '
import socket
a,b=socket.socketpair()
for name,opt in (("0x005 PEEREUUID",0x005),("0x006 PEERTOKEN",0x006)):
    try:
        v=a.getsockopt(0,opt,32); print(name,"len=",len(v),v.hex())
    except OSError as e:
        print(name,"ERROR",e)'
# 0x005 PEEREUUID len= 16 69908f7569853fbeb13d0a4c1db1e0ef
# 0x006 PEERTOKEN len= 32 f5010000f501000014000000f5010000140000008b000100b686010011790100
```

`audit_token()`（`peer.rs:147-167`）按 32 字节申请 `0x005`，内核只回 16 字节（剩下 16 字节保持 0），随后把这 32 字节当 audit token 交给 `SecCodeCopyGuestWithAttributes`（`peer.rs:261-299`）→ 不是合法 audit token → `verify_signature` 返回 Err → `authorize` 拒绝。也就是说：**`RequireSignature` 分支在真实构建上会把唯一合法的 App 也拒之门外；而生产构建又根本没编进这个分支**（F1）。第二条门的两种状态都是坏的。

补充：这段 FFI 在 CI 里从未被执行过，`peer.rs:22-27` 的模块注释自己写着"类型检查通过 ≠ 运行时正确，这里不能自欺欺人"；`peer.rs` 的 4 个测试只覆盖策略字符串与 `identify()`（`peer.rs:361-410`），**没有一条**走 `verify_signature`。

### F4（P1）`pem` 与 `fingerprint` 不做绑定；`existed_before` 只保证"不删证书"，不保证撤销我们改过的信任

- `crates/xt-helper/src/server.rs:401`：`install(pem, fingerprint)` 收到两个**互不相关**的字符串，helper 从不重算 `sha1(pem)` 并比对（全仓 `sha1_fingerprint` 只出现在 GUI 侧 `apps/desktop/src/mitm.rs:171,192,267` 与 `trust.rs` 测试）。调用方可以"装证书 A、记录指纹 B"。
- 后果 1：快照里记着 B，而钥匙串里多的是 A → 回滚删的是 B，A **永久留下且无记录**（正是 `server.rs:412-413` 注释里最怕的状态）。
- 后果 2：`remove_trust_anchor` 在快照里找不到指纹时**照样按调用方给的指纹删**（`crates/xt-helper/src/server.rs:464-473`），只补一句 note；`trust::remove` 走 `security delete-certificate -Z <指纹> System.keychain`（`crates/xt-tun/src/macos/trust.rs:82-89,226-246`）。配合 F1，就是一个"按任意已知指纹删除系统钥匙串条目"的 root 原语（需先有一个活跃 TUN 会话，`server.rs:441-446`；而 `TunUp` 同样可用）。
- 后果 3：`rollback()`（`trust.rs:248-266`）遇到 `existed_before == true` 直接返回 Ok，**不删证书**——这是对的——但它同时**不撤销我们那次 `add-trusted-cert -r trustRoot` 改掉的信任设置**。`is_trusted` 只判断"指纹在不在"（`trust.rs:139-158`），不判断"它的信任设置是不是我们写的"。所以"安装前就存在"的证书，如果原本不是 root，回滚后仍然是我们改成的 root。
- 为什么现有测试抓不到：`rollback_never_deletes_a_certificate_that_predates_us`（`trust.rs:381-398`）只断言 `existed_before=true` 时返回 Ok、非法指纹报错；`fingerprints_compare_case_and_separator_insensitively` 不涉及配对。helper 侧的 `installing_a_trust_anchor_without_a_session_is_refused_before_touching_the_keychain`（`server.rs:980`）只测"没有会话就拒"。

### F4-b（P3）`remove` 把英文 stderr 文案当"本来就不在"的判据

`crates/xt-tun/src/macos/trust.rs:236-244`：`stderr` 含 `"could not be found"`/`"not found"`/`"Unable to delete certificate matching"` 就当幂等成功。两个问题：① 本地化（`LC_ALL` 非英文）时这条判据失效 → 变成报错（偏安全方向）；② 任何**其它**错误信息里恰好含 `not found` 的情况都会被当成"删成功了"，而调用方（`rollback`/卸载路径）会据此认为信任锚已收回。没有测试覆盖这条分支。

### F5-b（P3）`fingerprint` 没有"必须是 40 位 hex"的形状要求

`crates/xt-tun/src/macos/trust.rs:115-133`：只要求非空、≤128、全是 hex/冒号、至少一个 hex。`AB` 也是合法指纹（再被 `normalize_fingerprint` 归一）。`install` 会为它写 `AB.pem` 并把 `AB` 交给 `security add-trusted-cert`（必然失败），`remove` 会拿 `AB` 去 `delete-certificate -Z`。这不是路径穿越（穿越已被挡住，测试 `a_malicious_fingerprint_is_refused` 在），但它让"合法指纹"的集合比 SHA-1 宽得多，也让"同一张证书两种写法"的归一化边界只靠大小写/冒号。

### F13（P2）安装脚本的 root 命令注入面 + 以 root 执行的临时脚本 TOCTOU

这两条是"helper 这条 root 通道"的入口，与信任锚机制同属一个攻击面（不在题目点名的 5 个文件里，但直接决定 root 权限的边界）：

- `apps/desktop/src/helper_install.rs:41-58`：`install_script()` 用 `HELPER_SRC='{helper_src}'` 把 `current_exe().parent()/xraytun-helper` 单引号插值进将**以 root 执行**的 `/bin/sh` 脚本。函数注释写"路径都来自我们自己的 app bundle，**不含任何用户输入**，所以不存在注入面"（`helper_install.rs:37-40`）——这句话只在"App 所在目录名里没有 `'`"时成立。App 从用户可写目录（下载目录等，或一个名字里带 `'` 的压缩包解出的目录）运行时，`'` 会闭合引号并让后半段成为 root 命令。本次**没有构造 PoC**，但这是纯字符串插值，判定不需要运行。
- `apps/desktop/src/helper_install.rs:239-259`：脚本用 `std::fs::write` 写到 `std::env::temp_dir()/xraytun-install-<pid>.sh`（`fs::write` 会跟随已存在的符号链接），随后 `restrict()` 设 0700，然后 `osascript ... with administrator privileges` 以 root 执行，执行后删除（`helper_install.rs:251-263`）。注释说"路径在用户不可写的目录下（`$TMPDIR` 是用户私有的），实际不可利用"（`helper_install.rs:248-250`）——`$TMPDIR` 的用户私有性挡的是**别的用户**，而模块自己的威胁模型是"同机上的非特权进程"（`crates/xt-helper/src/server.rs:5-6`），**同一个用户**的进程对该文件可写。从 `fs::write` 到 `osascript` 之间存在"以用户身份改写、以 root 身份执行"的 TOCTOU 窗口；文件名可由 pid 预测。同一条注释承认"理论上存在 TOCTOU 窗口"，但没有测试、也没有任何缓解（如 `open(O_EXCL)` + 保持 fd / `mkstemp` + `fchmod` / 拒绝已存在的路径）。

---

## 3. 信任锚的生命周期：幂等、回滚、快照

### F2（P0）helper 重启 / 机器重启后，`Up` 会话的信任锚不会被回滚，随后记录被新快照覆盖

链路（全部【代码可判】）：

1. helper 启动即 `recover_from_crash()`（`crates/xt-helper/src/main.rs:121` → `crates/xt-helper/src/server.rs:94-104`），它调 `controller::restore_stale()`。
2. `restore_stale()` 对 `!snap.is_stale()` **直接返回 Ok(None)**（`crates/xt-tun/src/macos/controller.rs:343-358`）；而 `is_stale()` 只认 `BringingUp | TearingDown | 还有 pending_routes`（`crates/xt-tun/src/macos/snapshot.rs:161-165`）。一条正常连上的会话是 `Up` 且 `pending_routes` 已清空（`snapshot.rs:267-295` 的测试名就写着 `a_committed_session_is_not_stale_but_still_needs_cleanup`）→ **不会回滚**。
3. 桌面端的"遗留会话"判据是 `snapshot.helper.stale_session.is_some() || snapshot.helper.tun_active`（`apps/desktop/src/lib.rs:337`）。`stale_session` 由 Hello 用同一个 `is_stale()` 过滤（`crates/xt-helper/src/server.rs:275-279`）；helper 新进程里 `tun_active == false`（会话在内存里，随进程死了，`server.rs:280-284`）。→ 两个都为假 ⇒ **不会调 `Request::Restore`**。
4. 下一次 `TunUp` → `controller::bring_up` 第一步就是 `SessionSnapshot::new(...)` 并 `snap.save()?`（`crates/xt-tun/src/macos/controller.rs:70-71`）→ **整份快照被覆盖**，旧的 `trust_anchors` 记录随之消失（`snapshot.rs:84-94` 定义了它、`server.rs:410` 往里 push）。
5. 结果：钥匙串里那张 CA 仍在、并且仍被信任；GUI 侧 `mitm_ca_remove` 只按**本会话** CA 的指纹删（`apps/desktop/src/commands/mitm.rs:92-95`），而重启后本会话 CA 还没生成（`existing_fingerprint()` 返回 `None`，`apps/desktop/src/mitm.rs:188-193`）→ 用户界面里没有任何操作能删掉旧指纹；helper 侧 `RemoveTrustAnchor` 又要求活跃会话（`server.rs:441-446`）。

触发场景很日常：**用户开着 MITM 用着网，然后机器重启**（helper 随系统退出，launchd `KeepAlive` 再拉起；会话状态是 `Up`），或 helper 被 `kill -9`。这与 `trust.rs` 模块文档"退出时（或下次启动的过期会话回滚）会把它删掉"（`apps/desktop/src/mitm.rs:16-19`）的承诺直接冲突。

为什么现有测试抓不到：`snapshot` 的测试只断言 `trust_anchors` 能 roundtrip、旧 JSON 能解析（`snapshot.rs:184-239`）；`recover_from_crash` 没有测试；helper 的用例只覆盖"没有会话时拒绝安装"。

复算（静态）：

```bash
grep -n "restore_stale\|force_cleanup" crates/xt-tun/src/macos/controller.rs crates/xt-helper/src/server.rs
grep -n "is_stale" -A 6 crates/xt-tun/src/macos/snapshot.rs
grep -n "stale_session\|tun_active" apps/desktop/src/lib.rs | head
grep -n "SessionSnapshot::new" crates/xt-tun/src/macos/controller.rs
```

### F6（P2）`is_trusted` 判的是"证书在不在"，不是"它是否被信任"

`crates/xt-tun/src/macos/trust.rs:139-158` 用 `security find-certificate -a -Z /Library/Keychains/System.keychain` 找 SHA-1 是否存在。**【运行时已验】**该命令输出确实能被解析（`SHA-1 hash: <40 hex>`）：

```bash
security find-certificate -a -Z /Library/Keychains/System.keychain | head -8
# SHA-256 hash: AF43...FDF
# SHA-1 hash: 24A6F4FC1A71B6AB70F665F07E7EBD2F89CF0047
# keychain: "/Library/Keychains/System.keychain"
# ...
security find-certificate -a -Z /Library/Keychains/System.keychain | grep -c "SHA-1 hash:"
# 3
```

但这只证明"证书存在"。用户在"钥匙串访问"里把这张根证书改成"永不信任"（或删掉信任设置、保留条目）之后，`is_trusted` 仍返回 `true` → `core_settings` 仍把 `mitm-steer` 下发给核心 → 被 steer 的域名撞上一张客户端拒绝的证书 = **用户看到"网站打不开"**，正是该闸门声称要避免的方向。系统层面有现成的判据可用（本机 `security dump-trust-settings -d` 可读，本次运行返回 `SecTrustSettingsCopyCertificates: No Trust Settings were found.`），代码没有用它，也没有 `security verify-cert` 之类的实际校验。没有测试覆盖"存在但不受信任"这一状态。

### F7（P2）安装/权限收紧的"失败静默"与"只在新文件生效"

- `crates/xt-tun/src/macos/trust.rs:269-277`：`set_dir_owner_only` 用 `let _ = std::fs::set_permissions(...)` **吞掉错误**；目录 `0700` 只是意图。
- `crates/xt-tun/src/macos/trust.rs:279-296`：`OpenOptions::mode(0o600)` 只在**创建**时生效；对已存在的同名文件（上一版留下的）不再收紧，且 `truncate(true)` 会复用旧 inode 的权限。同一模式也出现在快照（`crates/xt-tun/src/macos/snapshot.rs:168-171` 的 `set_owner_only` 同样吞错）。
- 这两个文件里放的是公开证书/DNS 备份，不是私钥，所以这里是"与文档口径不一致 + 失败不可见"，不是泄漏。复算：`cargo test -p xt-tun --lib trust` 全绿也照样抓不到，因为没有针对权限的断言。

---

## 4. MITM 的拆包范围

### F8（P1）`mitm.domains` 接受 Xray 规则语法 → "只拆用户点名的域名"可以被放大成"拆全部"

- 生成 steer 规则的函数：`crates/xt-core/src/xray/config.rs:100-108`

  ```rust
  s.mitm.domains.iter().map(|d| d.trim().to_ascii_lowercase())
      .filter(|d| !d.is_empty())
      .map(|d| if d.contains(':') { d } else { format!("full:{d}") })
  ```

  凡是**含 `:`** 的输入**原样**进入 `mitm-steer` 的 `domain` 列表（`config.rs:841-852`），而 Xray 的 domain 列表支持 `regexp:` / `domain:` / `geosite:`（`crates/xt-core/src/routing/mod.rs:77-87,592-594` 明确写着支持这些语法）。用户（或任何能写 `~/Library/Application Support/<APP_IDENTIFIER>/settings.json` 的同用户进程，路径见 `crates/xt-core/src/store.rs:41-51,67-69`）填入 `regexp:.*`，`is_active()` 只看"非空"（`crates/xt-core/src/model.rs:1134-1136`），于是**全量域名被 steer 进 MITM**——与 `apps/desktop/src/mitm.rs:21-28`"逐个域名由用户点"的设计声明冲突。
- 校验层没有拦住：
  - `MitmSettings::validate()`（`crates/xt-core/src/model.rs:1138-1168`）只查空名单/端口/body_strip，不检查域名形状。
  - 持久化入口 `AppSettings::validate()`（`crates/xt-core/src/model.rs:1325-1340`）**根本没有调用 `self.mitm.validate()`**，而它才是 `persist_settings` 用的那一个（`apps/desktop/src/state.rs:724-727`）。
  - UI 只按空白/逗号切分（`apps/ui/src/pages/Intent.tsx:297-302`），原样存回；界面显示的是"N 个域名"（`Intent.tsx:682`），不会提示它其实是正则。
- 现有测试抓不到：`crates/xt-core/src/xray/config.rs` 的 MITM 测试用的是普通域名（`config.rs:1320-1433`）。

复算：

```bash
grep -n "fn mitm_domains" -A 8 crates/xt-core/src/xray/config.rs
grep -n "fn is_active" -A 3 crates/xt-core/src/model.rs
grep -n "self.mitm" crates/xt-core/src/model.rs        # → AppSettings::validate 里没有它
grep -n "regexp:" crates/xt-core/src/routing/mod.rs crates/xt-core/src/xray/routing_api.rs | head
```

### F9（P2）本地代理端口没有认证，也不校验 SNI/Host 是否在 opt-in 名单里

- 监听只绑回环（正确）：`apps/desktop/src/mitm.rs:307-319` 把 `listen` 固定成 `127.0.0.1:<port>`；回连 socks 入站也只监听 `127.0.0.1`（`crates/xt-core/src/xray/config.rs:114-131`，`auth: noauth`）。
- 但 `handle_connection`（`crates/xt-mitm/src/proxy.rs:258-345`）**没有任何鉴权/端点白名单**：任何本地进程都能连 `127.0.0.1:<listen_port>` 并被当作 MITM 客户端；`CertResolver::resolve` 只要求有 SNI，且为**任意** SNI 现签叶子（`crates/xt-mitm/src/tls.rs:196-205`），从不比对 `settings.mitm.domains`。因此该端口同时是：① 一条"经用户隧道出去"的本地无认证代理（流量计入用户的代理链路）；② 一个"按任意域名签发、链到一张**系统已信任**的根"的签名端点（叶子私钥不外泄，所以不是密钥窃取）。
- `CertResolver` 的缓存**无上界**（`crates/xt-mitm/src/tls.rs:159-187`）：本地进程可以用不断变化的 SNI 反复连接，使缓存里的叶证书/私钥无限增长（进程生命周期内不淘汰）。连接并发上限 256（`proxy.rs:224-228`）不限制缓存总量。
- 为什么现有测试抓不到：`xt-mitm` 的集成测试都是"客户端本应如此"的正向路径；没有任何测试断言"不在名单里的 SNI 会被拒"（代码里也没有这条逻辑）。

复算：`cargo test -p xt-mitm` → 35+7 全绿（证明这些路径存在且现状如此）；`grep -n "domains" crates/xt-mitm/src/*.rs` → 只有 `proxy.rs` 注释，代码里没有任何名单比对。

---

## 5. fail-open：证书没被信任时，引导规则真的不下发吗？

### F3（P1）只有"核心启动"这一条路经过 `core_settings`

- 闸门实现：`apps/desktop/src/mitm.rs:45-51`（`ca_trusted=false` 时 `effective.mitm.enabled=false`）；唯一的生产调用点：`apps/desktop/src/commands/core.rs:191-194`。`mitm_apply` 的不起代理分支也对（`apps/desktop/src/commands/mitm.rs:138-143`）。
- **绕过点：`intent_apply` 热加整份规则表**（`apps/desktop/src/commands/intent.rs:124-168`）：

  ```rust
  let (needs, running, settings, (allow, block)) = snapshot;
  ...
  let rules = xt_core::xray::merge_rules_with_intent(&settings, &allow, &block); // 原始 settings
  ...
  match xt_core::xray::to_api_rules(&rules, &selected) { ... replace_rules(addr, &api_rules, ...) }
  ```

  这里的 `settings` 来自 `i.settings.clone()`，**没有经过 `core_settings`**（`grep -rn "core_settings" apps/desktop/src` 只有 `commands/core.rs:194` 一处 + `mitm.rs` 自身）。`merge_rules_with_intent` 在 `s.mitm.is_active()` 时无条件插入 `mitm-steer`（`crates/xt-core/src/xray/config.rs:831-852`）→ 一个"证书没装/没信任、`mitm-out` 出站根本不在运行配置里"的核心，会收到一条指向不存在出站的 steer 规则。Xray 对"outboundTag 不存在"的行为（报错拒收 vs 接受后逐连接失败）**本次没有实机验证**；但无论哪一种，都说明"引导规则绝不下发给核心"这条承诺只覆盖启动路径。
- 同一个闸门还有两个状态漏网：
  - `is_trusted` 只证明证书在（见 F6）。
  - `core_settings` 不检查"代理是否真的在跑"（`MitmRuntime::is_running` 在 `apps/desktop/src/mitm.rs:174-176`，闸门没用它）。用户在核心运行中点了"移除根证书"：`mitm_ca_remove` 会 `i.mitm.stop()`（`apps/desktop/src/commands/mitm.rs:114-117`），但**正在跑的核心里 steer 规则还在**（规则要重连才变），于是 opt-in 域名被 redirect 到 `127.0.0.1:<listen_port>` 的死端口（`crates/xt-core/src/xray/config.rs:576-582`）→ 连接被拒 = 用户看到"网站打不开"。界面只会说"引导规则要重连一次核心才会生效"（`apps/desktop/src/mitm.rs:250-255`），并不会阻止这段失败窗口。
- 现有测试只钉住了启动路径：`an_untrusted_ca_keeps_the_steering_rules_out_of_the_core_config`（`apps/desktop/src/mitm.rs:341-364`，本次实跑通过）测的是 `core_settings` + `merge_rules_with_intent` 的组合，测不到 `intent_apply` 这条独立调用链。

复算：

```bash
grep -rn "core_settings" apps/desktop/src
grep -n "merge_rules_with_intent" apps/desktop/src/commands/intent.rs crates/xt-core/src/xray/config.rs | head
sed -n '156,170p' apps/desktop/src/commands/intent.rs
```

---

## 6. SOCKS 形态、`assumed_port`、SNI/Host

### F14（P2）`assumed_port=443` 且 `Host` 里的端口被丢弃：证书按 SNI、回连按 Host、端口另算

- 生产把 `assumed_port` 写死 443（`apps/desktop/src/mitm.rs:318`，注释写明 `freedom.redirect` 不传目标）。
- 回连目的主机取 `head.host()`，而 `host()` 会**剥掉端口**（`crates/xt-mitm/src/http1.rs:120-128`：`h.rsplit_once(':')`），随后用 `cfg.assumed_port` 连接（`crates/xt-mitm/src/proxy.rs:400-405`）。
- TLS 身份来自 SNI（`crates/xt-mitm/src/tls.rs:196-205` 的 `hello.server_name()`），而判定与回连来自**Host 头**（`crates/xt-mitm/src/decide.rs:70-79`、`proxy.rs:400-405`）。代码里**没有任何一处**要求 `SNI == Host`，也不要求在 opt-in 名单内（见 F9）。
- 直接后果：`Host: svc.example:8443` 会被连到 `svc.example:443`（另一个服务），响应却按原样回给客户端；"同一个域名在域名层与内容层结论一致"的说法在 SNI/Host 不一致时不成立。

### F15（P3）`--socks5` vs `--socks5-hostname`：选择有据，但属于"用错就 fail-closed"的语义点

- 探测参数是纯函数、逐字可测：`apps/desktop/src/supervisor.rs:181-207`（`AtNode → --socks5-hostname`、`Locally → --socks5`），选择逻辑在 `supervisor.rs:912-919`：TUN 模式用 `--socks5`（本机解析，因为 TUN 下域名本就由核心 `dns-out` 解析），系统代理模式用 `--socks5-hostname`（节点解析）。单测在 `supervisor.rs:2515-2535`。
- 与 MITM 的关系：MITM 的回连走自己手写的 SOCKS5 CONNECT，用的是 **ATYP=3（域名）**（`crates/xt-mitm/src/proxy.rs:614-616`），即"域名交给上游 socks 入站"（等价 `socks5-hostname` 语义）→ 本机 DNS 不在这条回连路径上。
- 风险点是"选错就 fail-closed"：注释（`supervisor.rs:912-914`、`169-178`）明确写着用错会把本来可用的节点判死；这是产品语义而非实现缺陷，本次没有对两种模式各跑一次真实门禁来验证的选择正确性（见"我没能验证的"）。

---

## 7. 响应体裁剪与 `Content-Length` 一致性

### F16（P2）转发路径会发出"头声明 N、实际 body 少于 N"的响应

`read_response` 按 `Content-Length` 读体，读到 EOF 就 `break`，**不校验是否读满**，然后 `body.truncate(want)`（`crates/xt-mitm/src/proxy.rs:573-592`）；`exchange` 随后把**原始响应头**（原 `Content-Length`）与这个短 body 原样写回客户端（`proxy.rs:448-459`）。也就是说上游截断/超时时，MITM 会构造出 `Content-Length: N` 但只写 M<N 字节再关连接——正是 `rewrite.rs` 模块文档声称绝不会出现的那种不一致（`crates/xt-mitm/src/rewrite.rs:116-130` 的自检只作用于**我们改过 body** 的那条路径）。现有 7 条集成测试没有"上游少发字节"这个夹具。

### F17（P2）`set_header` 只改第一份 → 重复 `Content-Length` 可以带着不一致的长度出去

- `crates/xt-mitm/src/http1.rs:158-165`：`set_header` 只更新第一个匹配项；`remove_header` 才是删全部（`http1.rs:167-170`）。
- 裁剪写回走 `apply_body_change`：`remove_header("Transfer-Encoding")` + `set_header("Content-Length", ...)`（`crates/xt-mitm/src/rewrite.rs:98-111`）。若上游给了两个 `Content-Length`，第二个会保留旧值；`length_matches` 用 `get_header`（第一个）判定（`http1.rs:172-178`、`rewrite.rs:116-130`）→ 自检通过，但输出里同时存在新旧两个 CL，客户端取哪一个取决于实现。
- 请求侧同型：`read_head` 只读**第一个** `Content-Length` 给自己判断，却把全部头原样转发（`crates/xt-mitm/src/proxy.rs:381-388,409-417`）。

### F18（P2）请求侧只处理"无 body"形态，但没有拒绝 `Transfer-Encoding: chunked`

`read_head` 只在 `Content-Length > 0` 时报错（`crates/xt-mitm/src/proxy.rs:381-388`）；转发时只删 `accept-encoding`/`connection` 并写死 `Connection: close`（`proxy.rs:409-417`），**保留** `Transfer-Encoding`。于是一个 chunked 请求会被当作"无 body"转发，客户端后续字节会被 `read_head` 当成**下一条请求**解析（`proxy.rs:284-293` 的 keep-alive 循环）——这是 MITM 内部的请求走私/框架错配面。因连接已在 TLS 内、客户端就是本机进程，我把它定为 P2，但它是明确的协议解析缺口（"不读 body 却转发 body framing"）。本次**没有**做端到端走私 PoC。

### F19（P3）缓冲区上限与响应重构的代价

- 单个响应体上限 12 MiB（`crates/xt-mitm/src/proxy.rs:53,573-591`），并发连接上限 256（`proxy.rs:224`）⇒ 理论上限约 3 GiB 常驻缓冲；每个 opt-in 域名下的恶意/异常上游可以打到这个量级。
- 请求一律去掉 `Accept-Encoding`（`proxy.rs:409-415`）：这是为了长度一致性的刻意取舍，代价是所有被拆域名的传输失去压缩，且这个"改客户端请求"的行为对用户不可见。
- 裁剪只在 `body_strip` 配置齐全时生效（`apps/desktop/src/mitm.rs:75-85`、`crates/xt-core/src/model.rs:1107-1109`），且只处理 `application/json`、≤64 KiB（`crates/xt-mitm/src/rewrite.rs:232-245`）；失败一律原样转发——这部分的自检与负对照本次实跑通过（`cargo test -p xt-mitm` 的 `rewrite::tests::*`）。

---

## 8. `block-silent`

### F20（P2）配置路径映射正确，但**热加路径**把 Block 一律映射成 `block`

- 生成配置时：UDP-only 的 Block 走 `block-silent`（`crates/xt-core/src/routing/mod.rs:310-320`），出站定义在 `crates/xt-core/src/xray/config.rs:591-604`（`response.type: "none"`）；`mitm-quic-fallback` 是 `Network::Udp` 的 Block（`config.rs:853-867`）→ 配置路径上它是真静默。对应测试是 `crates/xt-core/src/xray/config.rs:1509-1560` 的 `udp_only_block_rules_go_to_a_silent_blackhole_outbound`（复算：`cargo test -p xt-core --lib udp_only_block_rules_go_to_a_silent_blackhole_outbound`；**本次运行在编译阶段被沙箱 60s 超时 kill，未取得结果**——这条我只做了代码判定）。
- 热加路径：`to_api_rules` 里 `RuleAction::Block => "block"` 是**无条件**的（`crates/xt-core/src/xray/routing_api.rs:647-652`），不看 `when.network`。于是 `intent_apply` 热加之后，UDP-only 规则与 `mitm-quic-fallback` 都变成 `block` → QUIC 客户端会收到那个本该被 `block-silent` 修掉的 403 数据报（`routing/mod.rs:312-318` 记录的正是这个故障）。这是一条"新机制把老 bug 请回来"的回归面，`routing_api.rs` 侧没有对应测试（`config.rs:1505-1560` 只测配置路径）。
- MITM 自己的阻断没有"静默"选项：命中黑名单固定回 `204 + Content-Length: 0 + X-XrayTun-Blocked: <原因> + Connection: close`（`crates/xt-mitm/src/decide.rs:82-103`），`proxy.rs:310-318` 直接用，然后关连接。原因字符串会按 RFC 净化所有控制字符（`decide.rs:94-98`，测试 `a_reason_with_newlines_cannot_inject_headers` 本次实跑通过）→ 头注入已挡住；但"被拦截"这件事对调用方是**可读的**（204 + 自定义头），不是静默丢弃。

复算：

```bash
grep -n "RuleAction::Block" crates/xt-core/src/routing/mod.rs crates/xt-core/src/xray/routing_api.rs
cargo test -p xt-mitm a_reason_with_newlines_cannot_inject_headers
```

---

## 9. 其它"看起来安全但本次无法证明安全"的点

- **`verify_signature` 的真实语义从未被执行**：`crates/xt-helper/src/peer.rs:22-27` 自述"类型检查通过 ≠ 运行时正确"；唯一的集成证据缺失（F1-b 已给出常量错误的实机证据）。
- **`XT_CA_DIR` 环境变量**（`crates/xt-tun/src/macos/trust.rs:43-44,61-66`）能改变 root 写文件的位置。生产 plist 只设 `XRAYTUN_LOG`（`apps/desktop/src/helper_install.rs:194-198`），launchd daemon 不继承用户环境，所以**我没有找到**生产可利用路径——但开发入口用 `sudo cargo run -p xt-helper`（`scripts/dev.sh:51`）会以 root 运行带用户环境的 helper，别把开发期的环境当成"生产也安全"的证据。
- **`XRAYTUN_HELPER_INSECURE=1` 运行时可关闭签名校验**（`crates/xt-helper/src/peer.rs:208-234`）：生产 plist 不设它（同上），但这意味着"签名校验"这个控制可以被**环境变量**整体关掉——值得在发布检查里显式断言二进制/plist 不含这条环境变量，目前没有这样的检查。
- **helper 的其它请求同样对同一批对端开放**：`Uninstall`（删 `/Library/PrivilegedHelperTools` 与 plist，`crates/xt-helper/src/server.rs:344`、`apps/desktop/src/helper_install.rs:142-159`）、`TunUp`（改本机路由/DNS）、`Shutdown`。在 F1 的前提下它们都是用户级进程可达的 root 动作，不只是信任锚问题。
- **文档与实现不一致处**（不影响安全结论，但会误导后续审计）：`crates/xt-tun/src/macos/trust.rs:38-41` 写"私钥永不出这个目录"，实际该目录从不出现私钥（私钥只在 GUI 内存里），helper 只收 PEM；`trust.rs:191-192` 写"任何一步失败都不是一半做完"，实际 `install()` 失败会留下 PEM 文件（F11）；`apps/desktop/src/commands/intent.rs:156` 写"顺序与生成配置时完全一致"，实际少了 MITM 闸门（F3）。

---

## 10. 我没能验证的

以下都不是"看着没事"，而是**本次没有取得运行时证据**，因此不能给出结论；我把它们明确留在这里：

1. **root 下的 `security(1)` 真实往返**：`real_install_and_remove_round_trip`（`crates/xt-tun/src/macos/trust.rs:427-441`）默认 `#[ignore]`（本次实跑输出：`ignored, 需要 root 与真实系统钥匙串；手动跑`）。未验证：`add-trusted-cert -d -r trustRoot -k System.keychain` 在真实钥匙串上的成功/失败形态、`is_trusted` 能否看到刚装的条目、`remove` 的英文文案判据在真实系统上的措辞。
2. **F1 的端到端利用**：未实际安装 helper、未以用户身份连接 socket 并下发 `InstallTrustAnchor`（需要真实安装好 helper 的机器与交互授权）；F1、F1-b 的结论来自代码 + 常量/返回长度的实机证据。
3. **F3 绕过点的真实后果**：未在"证书不可信 + 核心已在跑"的条件下点 `intent_apply`，因此**不知道** Xray 对"规则引用不存在出站 `mitm-out`"是拒收整个 AddRule 还是接受后逐连接失败。两种结果对结论（闸门被绕过）都成立，但用户可见症状不同。
4. **`mitm-out` 的 UDP 分支**：`Network::Both` 默认（`crates/xt-core/src/routing/mod.rs:20-25,100`）意味着 `mitm-steer` 也匹配 UDP，而代理只监听 TCP（`crates/xt-mitm/src/proxy.rs:192`），`freedom.redirect` 会经 UDP 把数据报送到 `127.0.0.1:<listen_port>`（`crates/xt-core/src/xray/config.rs:569-582` 注释自述 TCP/UDP 都走）。**我没有实机验证** opt-in 域名在 `block_quic=false` 时 UDP 是否被静默丢弃；如果成立，这是"没开 block_quic 也会丢 UDP"的 fail-closed 面。
5. **F9 的本地代理滥用**：未实际用本地进程连 MITM 端口做任意 SNI/任意 Host 的转发或让缓存无界增长；结论来自"没有鉴权代码 + resolver 只按 SNI 签发"。
6. **F18 的走私 PoC**、**F19 的 3 GiB 内存打满**、**F13 的两个 PoC**：均为静态判定，未构造可运行的攻击载荷。
7. **真实签名构建下的 `verify_signature`**：既没有带 Developer ID 签名的构建，也没有 `XRAYTUN_TEAM_ID` 注入的构建；`packaging` 明确是 ad-hoc 签名（`scripts/package-macos.sh:28-30,249`）。所以"注入了 Team ID 会怎样"只有 F1-b 的常量错误这一条间接证据。
8. **F6 的用户操作路径**：未在真实钥匙串上把一张已装证书改成"永不信任"再观察 `is_trusted` 的返回。
9. **`assumed_port` 非 443 场景**：桌面写死 443，未验证把 MITM 用在非 443 服务上的真实行为。
10. **本机是否装有旧版 helper / 线上机器当前的钥匙串状态**：没有 root，也没有对真实用户机器的访问；本报告只针对代码，不代表任何现网实例的当前状态。

---

## 附录 A：本次实际执行的复算命令

```bash
# 基线
cd /Users/xbtg-/deepseek-harness/xray-tun && git log --oneline -1     # 0e09214

# 私钥/密钥面
grep -rn "fs::\|File::create\|OpenOptions" crates/xt-mitm/src/*.rs
grep -rn "write_all" crates/xt-mitm/src/*.rs
grep -rn "serialize_pem\|serialize_der\|key_pem\|pkcs8" apps crates | grep -v tests/
find . -path ./target -prune -o -path ./apps/ui/node_modules -prune -o \
     \( -name "*.pem" -o -name "*.key" -o -name "*.p12" \) -print
git ls-files | grep -Ei '\.(pem|key|p12|pfx)$' | wc -l
git check-ignore -v weird.pem weird.key; echo "exit=$?"

# helper 授权面
grep -rn "XRAYTUN_TEAM_ID" .
SDK=$(xcrun --show-sdk-path); sed -n '85,95p' "$SDK/usr/include/sys/un.h"
python3 -c 'import socket; a,b=socket.socketpair();
for n,o in (("0x005",0x005),("0x006",0x006)):
    v=a.getsockopt(0,o,32); print(n,len(v),v.hex())'

# 回滚/快照
grep -n "restore_stale" -A 16 crates/xt-tun/src/macos/controller.rs
grep -n "SessionSnapshot::new" crates/xt-tun/src/macos/controller.rs
grep -n "is_stale" -A 6 crates/xt-tun/src/macos/snapshot.rs
grep -n "stale_session\|tun_active" apps/desktop/src/lib.rs | head

# fail-open 闸门
grep -rn "core_settings" apps/desktop/src
sed -n '156,170p' apps/desktop/src/commands/intent.rs

# 系统钥匙串读取（只读）
security find-certificate -a -Z /Library/Keychains/System.keychain | head -8
security dump-trust-settings -d

# 测试
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo cargo test -p xt-mitm
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo cargo test -p xt-tun --lib trust
CARGO_HOME=/Users/xbtg-/deepseek-harness/.cargo cargo test -p xraytun-desktop --lib mitm
```

（注：直接 `cargo test` 在本沙箱会因 `~/.cargo` 不可写而失败——`failed to open .../libc-*.crate: Operation not permitted`；上面命令用仓库旁的 `CARGO_HOME` 才跑通。）
