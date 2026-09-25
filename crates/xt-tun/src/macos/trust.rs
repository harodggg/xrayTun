//! 本地根证书的安装 / 移除 —— **MITM 阶段唯一会改系统状态的部分**。
//!
//! # 为什么它单独一个模块，而且每一步都要能单测
//!
//! 整个意图过滤功能里，其它部分要么只读日志、要么只改 Xray 自己的配置；
//! 只有这里会往**系统钥匙串**里塞一个我们生成的根证书。一旦出错，用户的信任面
//! 就被我们悄悄扩大了 —— 所以：
//!
//! * **参数构造是纯函数**（[`add_args`] / [`delete_args`]），逐字可测，
//!   不需要 root、不碰钥匙串；
//! * **PEM 形状与指纹先校验再动手**（[`validate_pem`] / [`validate_fingerprint`]），
//!   校验不过就**在写任何文件之前**失败；
//! * **删除是幂等的**：证书已经被用户手动删掉时不算失败（回滚最怕"删不到就卡住"）；
//! * 真正执行 `security(1)` 的路径用 `#[ignore]` 的真实用例覆盖（**需要 root**），
//!   默认不跑 —— 但"没跑"这件事必须写在用例里，不能假装跑过。
//!
//! # 两个踩过的坑（写在这里，免得下次又踩）
//!
//! 1. **绝对路径 `/usr/bin/security`**：PATH 上可能先命中别的同名包装
//!    （README 里记过 `xattr` 的同类事故：`xattr -r` 在 Python 的实现上直接 exit 64）。
//! 2. **指纹要参与路径**：指纹来自调用方，可能被构造成 `../../etc/foo`。
//!    所以 [`validate_fingerprint`] 只允许十六进制与冒号 —— 路径穿越在这里
//!    不是理论风险，而是"helper 以 root 运行"下的提权面。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// `security(1)` 的**绝对路径**（见模块文档第 1 条）。
pub const SECURITY: &str = "/usr/bin/security";

/// 系统钥匙串。
pub const SYSTEM_KEYCHAIN: &str = "/Library/Keychains/System.keychain";

/// 信任锚落盘目录（0600 文件 / 0700 目录）。
///
/// 与 helper 的其它状态放在一起：**私钥永不出这个目录**，下发给 GUI 的只有 PEM。
pub const DEFAULT_CA_DIR: &str = "/Library/Application Support/XrayTun/ca";

/// 环境变量覆盖（测试与开发用；生产不设它）。
pub const CA_DIR_ENV: &str = "XT_CA_DIR";

/// 一次安装的备份记录 —— **回滚要用的就是它**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAnchorBackup {
    /// SHA-1 指纹（`security delete-certificate -Z` 用的就是它）。
    pub fingerprint: String,
    /// 证书在 helper 目录里的绝对路径。
    pub cert_path: String,
    /// 安装**之前**这个指纹就已经被信任了吗。
    ///
    /// `true` ⇒ 回滚时**不该删**它（用户本来就有），否则我们会把一个用户自己的
    /// 证书从钥匙串里删掉。
    pub existed_before: bool,
}

/// 信任锚的安装目录（可被 [`CA_DIR_ENV`] 覆盖）。
pub fn ca_dir() -> PathBuf {
    match std::env::var(CA_DIR_ENV) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from(DEFAULT_CA_DIR),
    }
}

/// `security add-trusted-cert` 的**完整参数**（不含程序名）。
pub fn add_args(cert_path: &str) -> Vec<String> {
    vec![
        "add-trusted-cert".into(),
        "-d".into(),          // 加到 admin（系统）域
        "-r".into(),
        "trustRoot".into(),   // 作为根
        "-k".into(),
        SYSTEM_KEYCHAIN.into(),
        cert_path.into(),
    ]
}

/// `security delete-certificate` 的**完整参数**（不含程序名）。
pub fn delete_args(fingerprint: &str) -> Vec<String> {
    vec![
        "delete-certificate".into(),
        "-Z".into(), // 按 SHA-1 指纹删
        fingerprint.into(),
        SYSTEM_KEYCHAIN.into(),
    ]
}

/// PEM 必须是**一张证书**：只做形状检查（不做密码学校验 —— 那由 GUI 侧生成时保证）。
///
/// 形状检查不是形式主义：把一段私钥、或半截文件塞进系统钥匙串，
/// 后果是"信任了一个我们不认识的东西"。
pub fn validate_pem(pem: &str) -> Result<()> {
    if pem.trim().is_empty() {
        return Err(Error::Invalid("证书 PEM 是空的".into()));
    }
    if !pem.contains("-----BEGIN CERTIFICATE-----") || !pem.contains("-----END CERTIFICATE-----") {
        return Err(Error::Invalid("PEM 里没有完整的 CERTIFICATE 块".into()));
    }
    // 私钥**绝不允许**出现在这里：它只该待在 helper 目录里。
    for banned in ["PRIVATE KEY", "PRIVATE KEY-----"] {
        if pem.contains(banned) {
            return Err(Error::Invalid(format!("PEM 里含私钥（{banned}）—— 拒绝写入系统钥匙串")));
        }
    }
    Ok(())
}

/// 指纹只允许十六进制与冒号（形如 `AB:CD:...` 或纯 hex）。
///
/// **这是安全边界**：指纹会参与文件名（`<fp>.pem`），允许 `/` 或 `.` 就是路径穿越，
/// 而 helper 以 root 运行。
pub fn validate_fingerprint(fingerprint: &str) -> Result<()> {
    let f = fingerprint.trim();
    if f.is_empty() {
        return Err(Error::Invalid("指纹是空的".into()));
    }
    if f.len() > 128 {
        return Err(Error::Invalid("指纹过长".into()));
    }
    let ok = f
        .chars()
        .all(|c| c.is_ascii_hexdigit() || c == ':');
    if !ok {
        return Err(Error::Invalid(format!("指纹只能含十六进制与冒号，现在是 {f:?}")));
    }
    if !f.chars().any(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Invalid("指纹里没有任何十六进制字符".into()));
    }
    Ok(())
}

/// 跑一次 `security(1)`，拿回原始输出。
///
/// 抽出来**只为可测**：回滚路径要执行 `security delete-certificate`，而
/// 「旧指纹到底有没有被交给 `security`」必须有断言（P0：孤儿信任锚删不掉）。
/// 生产构建里没有任何注入点，就是 [`std::process::Command`]，行为与以前逐字一致。
fn security_output(args: &[String]) -> std::io::Result<std::process::Output> {
    #[cfg(test)]
    {
        if let Some(stub) = current_security_stub() {
            let (ok, stdout, stderr) = stub(args);
            return Ok(stub_output(ok, stdout, stderr));
        }
    }
    Command::new(SECURITY).args(args).output()
}

/// 测试替身：收到参数，回 `(是否成功, stdout, stderr)`。
///
/// 单独起别名有两个理由：让接缝形状可读；避免 `clippy::type_complexity`
/// （本仓 `-D warnings`）。
#[cfg(test)]
pub(crate) type SecurityStub = std::rc::Rc<dyn Fn(&[String]) -> (bool, Vec<u8>, Vec<u8>)>;

#[cfg(test)]
thread_local! {
    /// 测试注入的 `security(1)` 替身（**只在测试构建里存在**）。
    static SECURITY_STUB: std::cell::RefCell<Option<SecurityStub>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn current_security_stub() -> Option<SecurityStub> {
    SECURITY_STUB.with(|c| c.borrow().clone())
}

/// 在 `f()` 期间把 `security(1)` 换成 `stub`；退出（含 panic）自动还原。
#[cfg(test)]
pub(crate) fn with_security_stub<R>(stub: SecurityStub, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<SecurityStub>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SECURITY_STUB.with(|c| *c.borrow_mut() = self.0.take());
        }
    }
    let prev = SECURITY_STUB.with(|c| c.borrow_mut().replace(stub));
    let _guard = Restore(prev);
    f()
}

#[cfg(test)]
fn stub_output(ok: bool, stdout: Vec<u8>, stderr: Vec<u8>) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    std::process::Output {
        // `from_raw(0)` = 正常退出；`from_raw(1 << 8)` = 退出码 1。
        status: std::process::ExitStatus::from_raw(if ok { 0 } else { 1 << 8 }),
        stdout,
        stderr,
    }
}

/// 是否已经信任了这个指纹。
///
/// 用 `security find-certificate -Z` 列出**全部**证书的 SHA-1 再比对：
/// 找不到就是"没信任过"（`existed_before=false`）。
pub fn is_trusted(fingerprint: &str) -> Result<bool> {
    validate_fingerprint(fingerprint)?;
    let want = normalize_fingerprint(fingerprint);
    let out = security_output(&crate::macos::args(&[
        "find-certificate",
        "-a",
        "-Z",
        SYSTEM_KEYCHAIN,
    ]))
    .map_err(|e| Error::Invalid(format!("跑 {SECURITY} 失败: {e}")))?;
    if !out.status.success() {
        // 读不出来 ⇒ **不许猜成 false**（那会让回滚去删一个本来不该删的东西的邻域）。
        return Err(Error::Invalid(format!(
            "读系统钥匙串失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .filter_map(|l| l.split_once("SHA-1 hash:").map(|(_, v)| v.trim()))
        .any(|v| normalize_fingerprint(v) == want))
}

/// 一段 DER 的 **SHA-1 指纹**（大写、冒号分隔，形如 `AB:CD:...`）。
///
/// # 为什么是 SHA-1
///
/// 不是我们选的：`security delete-certificate -Z` 与
/// `security find-certificate -Z` 用的就是 SHA-1，而删证书**必须**用同一个值
/// 才能删对（[`delete_args`]）。所以这里的算法由 `security(1)` 决定。
///
/// 这不是签名，也不承担防碰撞职责：它只是一个**钥匙串条目的定位符**，
/// 证书本身的可信性由钥匙串里的 DER 决定。
pub fn sha1_fingerprint(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, der);
    digest
        .as_ref()
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// 统一成大写无冒号形式，避免"同一个指纹两种写法"被当成两个。
pub fn normalize_fingerprint(fingerprint: &str) -> String {
    fingerprint
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// 安装信任锚。
///
/// 顺序是刻意的：**校验 → 写文件 → 才动钥匙串**。任何一步失败都留下可读的错误，
/// 而不是"一半做完了"。
pub fn install(pem: &str, fingerprint: &str) -> Result<TrustAnchorBackup> {
    validate_pem(pem)?;
    validate_fingerprint(fingerprint)?;

    let existed_before = is_trusted(fingerprint)?;
    let dir = ca_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| Error::Invalid(format!("创建 {} 失败: {e}", dir.display())))?;
    set_dir_owner_only(&dir);

    let cert_path = dir.join(format!("{}.pem", normalize_fingerprint(fingerprint)));
    write_owner_only(&cert_path, pem.as_bytes())?;

    let args = add_args(&cert_path.to_string_lossy());
    let out = security_output(&args)
        .map_err(|e| Error::Invalid(format!("跑 {SECURITY} 失败: {e}")))?;
    if !out.status.success() {
        return Err(Error::Invalid(format!(
            "add-trusted-cert 失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    Ok(TrustAnchorBackup {
        fingerprint: normalize_fingerprint(fingerprint),
        cert_path: cert_path.to_string_lossy().to_string(),
        existed_before,
    })
}

/// 移除信任锚。**幂等**：本来就不在 ⇒ `Ok`（回滚最怕"删不到就卡住"）。
pub fn remove(fingerprint: &str) -> Result<()> {
    validate_fingerprint(fingerprint)?;
    let out = security_output(&delete_args(fingerprint))
        .map_err(|e| Error::Invalid(format!("跑 {SECURITY} 失败: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    // `delete-certificate` 对"找不到"的措辞在不同 macOS 版本上不一致，
    // 所以判据放宽成"提到 not found / 找不到就当我成功"——**但要留痕**。
    let missing = stderr.contains("could not be found")
        || stderr.contains("not found")
        || stderr.contains("Unable to delete certificate matching");
    if missing {
        tracing::info!(fingerprint, "信任锚本来就不在钥匙串里（幂等成功）");
        return Ok(());
    }
    Err(Error::Invalid(format!("delete-certificate 失败：{}", stderr.trim())))
}

/// 按**备份记录**回滚：只有"安装之前不存在"的才删。
pub fn rollback(backup: &TrustAnchorBackup) -> Result<()> {
    if backup.existed_before {
        tracing::info!(
            fingerprint = %backup.fingerprint,
            "这个信任锚在安装前就存在 ⇒ 回滚时不删（那是用户自己的证书）"
        );
        return Ok(());
    }
    remove(&backup.fingerprint)?;
    // 文件也删掉：留着只会让"我们装过什么"变得难以判断。
    let path = Path::new(&backup.cert_path);
    if path.exists() {
        if let Err(e) = std::fs::remove_file(path) {
            tracing::warn!(path = %path.display(), error = %e, "删证书文件失败（钥匙串已经撤了，影响有限）");
        }
    }
    Ok(())
}

/// 权限收紧：目录 0700 / 文件 0600。
fn set_dir_owner_only(dir: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = dir;
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // **权限在写入内容之前就已收紧**（与 xt-core 的落盘口径一致）。
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| Error::Invalid(format!("写 {} 失败: {e}", path.display())))?;
    f.write_all(bytes)
        .map_err(|e| Error::Invalid(format!("写 {} 失败: {e}", path.display())))?;
    f.flush()
        .map_err(|e| Error::Invalid(format!("写 {} 失败: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";

    /// 指纹会进文件名，而 helper 以 root 跑 ⇒ 路径穿越是**提权面**，不是理论风险。
    #[test]
    fn a_malicious_fingerprint_is_refused() {
        for bad in [
            "../../../etc/passwd",
            "ab/cd",
            "ab.cd",
            "AB CD",
            "ab;rm -rf /",
            "",
            "   ",
        ] {
            assert!(
                validate_fingerprint(bad).is_err(),
                "{bad:?} 居然通过了指纹校验"
            );
        }
        // 合法的两种写法都要接受。
        assert!(validate_fingerprint("AB:CD:EF:01").is_ok());
        assert!(validate_fingerprint("abcdef01").is_ok());
    }

    #[test]
    fn fingerprints_compare_case_and_separator_insensitively() {
        assert_eq!(normalize_fingerprint("ab:cd:ef"), normalize_fingerprint("ABCDEF"));
        assert_eq!(normalize_fingerprint("AB CD EF"), "ABCDEF");
    }

    /// 私钥**绝不允许**进系统钥匙串。
    #[test]
    fn a_pem_with_a_private_key_or_without_a_cert_block_is_refused() {
        assert!(validate_pem(GOOD_PEM).is_ok());
        assert!(validate_pem("").is_err());
        assert!(validate_pem("-----BEGIN PRIVATE KEY-----\nx\n-----END PRIVATE KEY-----\n").is_err());
        assert!(validate_pem("-----BEGIN CERTIFICATE-----\nMIIB\n").is_err(), "只有头没有尾");
        assert!(
            validate_pem(&format!("{GOOD_PEM}-----BEGIN PRIVATE KEY-----\nx\n")).is_err(),
            "证书块 + 私钥块也必须拒（私钥永远不进钥匙串）"
        );
    }

    /// 参数必须逐字正确：`-d`（系统域）/ `-r trustRoot` / 绝对路径的 security 与钥匙串。
    #[test]
    fn the_security_arguments_are_exactly_what_we_verified() {
        assert_eq!(SECURITY, "/usr/bin/security");
        assert_eq!(
            add_args("/Library/Application Support/XrayTun/ca/AB.pem"),
            vec![
                "add-trusted-cert",
                "-d",
                "-r",
                "trustRoot",
                "-k",
                "/Library/Keychains/System.keychain",
                "/Library/Application Support/XrayTun/ca/AB.pem",
            ]
        );
        assert_eq!(
            delete_args("AB:CD"),
            vec!["delete-certificate", "-Z", "AB:CD", "/Library/Keychains/System.keychain"]
        );
    }

    /// 校验不过时**不许写任何文件**（"先校验再动手"的顺序要求）。
    #[test]
    fn a_bad_pem_fails_before_touching_the_filesystem() {
        let dir = std::env::temp_dir().join(format!("xt-ca-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY：单测进程内串行改环境变量；这个测试不依赖并行度。
        std::env::set_var(CA_DIR_ENV, &dir);
        assert!(install("not a certificate", "AB:CD").is_err());
        assert!(!dir.exists(), "校验失败却建了目录：{}", dir.display());
        std::env::remove_var(CA_DIR_ENV);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回滚必须**区分**"我们装的"与"本来就有"的信任锚。
    #[test]
    fn rollback_never_deletes_a_certificate_that_predates_us() {
        let preexisting = TrustAnchorBackup {
            fingerprint: "AB:CD".into(),
            cert_path: "/nonexistent/path.pem".into(),
            existed_before: true,
        };
        // 提前存在 ⇒ 直接 Ok，不去调 security（也就不会真的删用户的证书）。
        assert!(rollback(&preexisting).is_ok());

        // 非法指纹同样被挡在参数构造之前。
        let broken = TrustAnchorBackup {
            fingerprint: "../x".into(),
            cert_path: "/tmp/x.pem".into(),
            existed_before: false,
        };
        assert!(rollback(&broken).is_err());
    }

    /// SHA-1 指纹：**用公开测试向量钉住**（不是"跑一遍看输出"）。
    ///
    /// 这条同时钉住三件事：算法是 SHA-1、格式是大写冒号分隔、而且真的过了
    /// [`validate_fingerprint`]（helper 会拿它当文件名，格式不对就是提权面）。
    #[test]
    fn the_sha1_fingerprint_matches_a_published_test_vector() {
        // SHA-1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        let fp = sha1_fingerprint(b"abc");
        assert_eq!(
            fp,
            "A9:99:3E:36:47:06:81:6A:BA:3E:25:71:78:50:C2:6C:9C:D0:D8:9D"
        );
        validate_fingerprint(&fp).expect("我们自己产出的指纹必须能通过校验");
        assert_eq!(normalize_fingerprint(&fp).len(), 40);
        // 负对照：改一个字节就必须变（否则"指纹"定位不到条目）。
        let mut other = b"abd".to_vec();
        other[2] = b'd';
        assert_ne!(sha1_fingerprint(&other), fp);
        // 也与"另一张 CA"不同：DER 前缀一样但内容不同。
        assert_ne!(sha1_fingerprint(&[]), fp);
    }

    /// **需要 root 的真实用例。** 默认 `#[ignore]` —— 没跑就是没跑，不许假装跑过。
    ///
    /// ```bash
    /// sudo -E cargo test -p xt-tun --lib trust -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "需要 root 与真实系统钥匙串；手动跑"]
    fn real_install_and_remove_round_trip() {
        // 用一张自签证书（由 GUI 侧生成）；这里只在有 root 时验证
        // `security` 的调用路径与"装完能查到、删完查不到"。
        let pem = std::env::var("XT_TEST_CA_PEM").expect("需要 XT_TEST_CA_PEM 指向一个 PEM 文件");
        let fp = std::env::var("XT_TEST_CA_FP").expect("需要 XT_TEST_CA_FP（SHA-1 指纹）");
        let pem_text = std::fs::read_to_string(&pem).expect("读 PEM");
        let backup = install(&pem_text, &fp).expect("安装应当成功");
        assert!(is_trusted(&fp).expect("查询"), "装完必须能查到");
        rollback(&backup).expect("回滚应当成功");
        if !backup.existed_before {
            assert!(!is_trusted(&fp).expect("查询"), "回滚之后不该还在");
        }
    }
}
