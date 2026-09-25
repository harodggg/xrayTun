//! 本地 CA 与叶子证书、以及**只广告 http/1.1** 的 TLS 配置。
//!
//! # 只广告 `http/1.1` 是刻意的降复杂度决定
//!
//! 不广告 `h2` ⇒ 被 opt-in 的域名一律退化成 HTTP/1.1：
//!
//! * 我们只需要解析 h1 的请求行与头，不必实现 h2 的帧/流/流控（那是 MITM 里最重的一块）；
//! * 代价要写进界面文案：**opt-in 域名失去 HTTP/2 多路复用**，可能与直连表现不同；
//! * WebSocket（`Upgrade: websocket`）盲转发，不解析。
//!
//! # 私钥在这里，但**信任锚不在这里**
//!
//! 本模块生成的 CA**只存在于内存**（每次进程启动新生成），真正被系统信任的那张
//! 由 helper 装进钥匙串（见 `xt-tun::macos::trust`）。也就是说：
//! **MITM 进程重启后 CA 会变**，而钥匙串里那张要等 helper 回滚才会消失 ——
//! 所以"重启 MITM"必须同时重装信任锚，这条约束由调用方保证（下一步接线时会写进注释）。
//!
//! # 叶子证书短效
//!
//! 每个域名一张、有效期 **7 天**（本模块只在内存里留，重启即重签）。
//! 长效期没有意义：它增加的是"私钥泄露之后还能用多久"。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

/// TLS 相关的错误。**每一种都要能读**（证书生成失败是最难猜的一类）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsError {
    /// 证书/私钥生成失败。
    Cert(String),
    /// 域名不合法（空、含控制字符…）——**拿它去签证书等于伪造别人**，必须拒绝。
    BadHost(String),
    /// rustls 配置失败。
    Config(String),
}

impl std::fmt::Display for TlsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cert(e) => write!(f, "生成证书失败：{e}"),
            Self::BadHost(h) => write!(f, "域名不合法，拒绝为它签证书：{h:?}"),
            Self::Config(e) => write!(f, "TLS 配置失败：{e}"),
        }
    }
}

impl std::error::Error for TlsError {}

/// 只广告 HTTP/1.1（见模块文档）。**不要往里加 `h2`**。
pub const ALPN_HTTP1: &[u8] = b"http/1.1";

/// 叶子证书有效期（天）。
pub const LEAF_DAYS: i64 = 7;

/// 根证书有效期（天）。
///
/// ⚠️ **必须显式设置**：rcgen 的默认是 `1975-01-01 → 4096-01-01`（等于永不过期）。
/// 根证书私钥一旦泄露（或被备份/同步带走），"永不过期"意味着无法靠时间化解 ——
/// 只能逐台机器手动删。2 年是"够用 + 可轮换"的折中；到期日会通过
/// `MitmStatus::ca_expires_at` 回传给界面，让轮换这件事可见。
pub const CA_DAYS: i64 = 730;

/// 内存里的本地 CA。
pub struct LocalCa {
    cert: rcgen::Certificate,
    key: KeyPair,
    cert_pem: String,
    /// 生效/到期时刻（UTC）。显式设置的原因见 [`CA_DAYS`]。
    not_before: time::OffsetDateTime,
    not_after: time::OffsetDateTime,
}

impl LocalCa {
    /// 生成一张自签 CA（CN = `XrayTun Local CA`）。
    pub fn generate() -> Result<Self, TlsError> {
        let key = KeyPair::generate().map_err(|e| TlsError::Cert(e.to_string()))?;
        let mut params = CertificateParams::new(Vec::<String>::new())
            .map_err(|e| TlsError::Cert(e.to_string()))?;
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params
            .distinguished_name
            .push(DnType::CommonName, "XrayTun Local CA");
        // 有效期必须显式给：不吃 rcgen 的 1975→4096 默认值。
        // `not_before` 回拨一天，容忍客户端与本机的时钟偏移（否则刚签出来就可能
        // 被判成"尚未生效"）。
        let now = time::OffsetDateTime::now_utc();
        let not_before = now - time::Duration::days(1);
        let not_after = now + time::Duration::days(CA_DAYS);
        params.not_before = not_before;
        params.not_after = not_after;
        let cert = params
            .self_signed(&key)
            .map_err(|e| TlsError::Cert(e.to_string()))?;
        let cert_pem = cert.pem();
        Ok(Self {
            cert,
            key,
            cert_pem,
            not_before,
            not_after,
        })
    }

    /// CA 的 PEM（交给 helper 装进钥匙串的就是它）。
    pub fn cert_pem(&self) -> &str {
        &self.cert_pem
    }

    /// 生效时刻（UTC）。
    pub fn not_before(&self) -> time::OffsetDateTime {
        self.not_before
    }

    /// 到期时刻（UTC）。
    pub fn not_after(&self) -> time::OffsetDateTime {
        self.not_after
    }

    /// 到期日 `YYYY-MM-DD`（给 `MitmStatus` 用，避免把 `time` 类型泄到 app 层）。
    pub fn expiry_ymd(&self) -> String {
        let d = self.not_after;
        format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
    }

    /// CA 的 DER（rustls 的根仓库只吃 DER）。
    ///
    /// 测试客户端要用**同一张** CA 去信这个代理；生产里 helper 装进钥匙串的
    /// 是同一份证书，所以"测试信任"与"系统信任"不是两套东西。
    pub fn cert_der(&self) -> CertificateDer<'static> {
        self.cert.der().clone()
    }

    /// 为某个域名签一张短效叶子证书，并直接构造好 rustls 的服务端配置。
    pub fn server_config(&self, host: &str) -> Result<Arc<rustls::ServerConfig>, TlsError> {
        let (chain, key) = self.leaf_for(host)?;
        let mut cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| TlsError::Config(e.to_string()))?;
        cfg.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
        Ok(Arc::new(cfg))
    }

    /// 用**按 SNI 现签**的 resolver 构造服务端配置（代理用这一条）。
    pub fn server_config_with_resolver(
        &self,
        resolver: Arc<CertResolver>,
    ) -> Result<Arc<rustls::ServerConfig>, TlsError> {
        let mut cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        cfg.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
        Ok(Arc::new(cfg))
    }

    fn leaf_for(
        &self,
        host: &str,
    ) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), TlsError> {
        let host = host.trim().trim_end_matches('.');
        if host.is_empty() || host.chars().any(|c| c.is_control()) || host.len() > 253 {
            return Err(TlsError::BadHost(host.to_string()));
        }
        let leaf_key = KeyPair::generate().map_err(|e| TlsError::Cert(e.to_string()))?;
        let mut params = CertificateParams::new(vec![host.to_string()])
            .map_err(|e| TlsError::Cert(e.to_string()))?;
        params
            .distinguished_name
            .push(DnType::CommonName, host.to_string());
        let leaf = params
            .signed_by(&leaf_key, &self.cert, &self.key)
            .map_err(|e| TlsError::Cert(e.to_string()))?;

        let chain = vec![
            CertificateDer::from(leaf.der().to_vec()),
            CertificateDer::from(self.cert.der().to_vec()),
        ];
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der()));
        Ok((chain, key))
    }
}

impl std::fmt::Debug for LocalCa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // **绝不打印私钥**（哪怕在 Debug 里）。
        f.debug_struct("LocalCa")
            .field("cert_pem_len", &self.cert_pem.len())
            .finish()
    }
}

/// 按 SNI 现签（并缓存）叶子证书的解析器。
///
/// # 为什么要按 SNI 现签
///
/// steer 过来的连接已经丢了原始目标（`freedom.redirect` 不传递它），
/// **SNI 是我们唯一能拿到主机名的地方**。所以证书必须按 ClientHello 里的名字签，
/// 否则客户端（用系统根校验）会直接报证书不匹配。
pub struct CertResolver {
    ca: Arc<LocalCa>,
    cache: Mutex<HashMap<String, Arc<rustls::sign::CertifiedKey>>>,
}

impl CertResolver {
    pub fn new(ca: Arc<LocalCa>) -> Self {
        Self {
            ca,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// 已为多少个域名签过证书（诊断用 —— 也是"MITM 到底在看哪些站"的可见面）。
    pub fn cached_hosts(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }

    fn key_for(&self, host: &str) -> Option<Arc<rustls::sign::CertifiedKey>> {
        if let Ok(cache) = self.cache.lock() {
            if let Some(k) = cache.get(host) {
                return Some(k.clone());
            }
        }
        let (chain, key) = self.ca.leaf_for(host).ok()?;
        let signing = rustls::crypto::ring::sign::any_supported_type(&key).ok()?;
        let ck = Arc::new(rustls::sign::CertifiedKey::new(chain, signing));
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(host.to_string(), ck.clone());
        }
        Some(ck)
    }
}

impl std::fmt::Debug for CertResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertResolver")
            .field("cached_hosts", &self.cached_hosts())
            .finish()
    }
}

impl rustls::server::ResolvesServerCert for CertResolver {
    fn resolve(
        &self,
        hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        // 没有 SNI 就签不出来 —— 返回 None 让握手失败。
        // **不许**退回一张"万能证书"：那会把不匹配的域名也伪装成可信。
        let host = hello.server_name()?;
        self.key_for(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ca_can_be_generated_and_is_a_valid_pem() {
        let ca = LocalCa::generate().unwrap();
        assert!(ca.cert_pem().starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(ca.cert_pem().contains("-----END CERTIFICATE-----"));
        // Debug 里不许出现私钥材料。
        let dbg = format!("{ca:?}");
        assert!(!dbg.contains("PRIVATE"), "{dbg}");
        assert!(!dbg.contains("BEGIN"), "{dbg}");
    }

    /// 根证书有效期必须**有界**：rcgen 的默认（1975→4096）等于永不过期。
    #[test]
    fn a_ca_validity_is_bounded_not_the_rcgen_default() {
        let ca = LocalCa::generate().unwrap();
        let span = ca.not_after() - ca.not_before();
        assert_eq!(
            span.whole_days(),
            CA_DAYS + 1,
            "not_before 回拨 1 天 ⇒ 跨度是 CA_DAYS+1"
        );
        assert!(
            ca.not_after().year() < 2200,
            "不许落到 rcgen 的 4096 默认值：{}",
            ca.expiry_ymd()
        );
        assert_eq!(ca.expiry_ymd().len(), 10, "{}", ca.expiry_ymd());
    }

    #[test]
    fn a_server_config_only_advertises_http1() {
        let ca = LocalCa::generate().unwrap();
        let cfg = ca.server_config("news.example").unwrap();
        assert_eq!(cfg.alpn_protocols, vec![ALPN_HTTP1.to_vec()]);
        assert!(
            !cfg.alpn_protocols.iter().any(|p| p == b"h2"),
            "不许广告 h2"
        );
    }

    /// 非法域名**不许**被签 —— 拿它签等于替别人伪造身份。
    #[test]
    fn an_invalid_host_is_refused() {
        let ca = LocalCa::generate().unwrap();
        assert!(matches!(ca.server_config(""), Err(TlsError::BadHost(_))));
        assert!(matches!(
            ca.server_config("evil\r\nhost"),
            Err(TlsError::BadHost(_))
        ));
        assert!(matches!(
            ca.server_config(&"a".repeat(300)),
            Err(TlsError::BadHost(_))
        ));
    }

    #[test]
    fn the_resolver_caches_per_host_and_reuses_it() {
        let ca = Arc::new(LocalCa::generate().unwrap());
        let r = CertResolver::new(ca);
        assert_eq!(r.cached_hosts(), 0);
        let a = r.key_for("a.example").expect("签得出来");
        let b = r.key_for("a.example").expect("第二次应当命中缓存");
        assert!(Arc::ptr_eq(&a, &b), "同一个域名应当复用同一张叶子证书");
        let _ = r.key_for("b.example");
        assert_eq!(r.cached_hosts(), 2);
    }

    /// **判别性**：`cert_der()` 出来的东西必须能进 rustls 的根仓库
    /// （测试客户端就是靠它信这个代理的；错一个字节就握不上手）。
    #[test]
    fn the_ca_der_can_be_used_as_a_trust_root() {
        let ca = LocalCa::generate().unwrap();
        let der = ca.cert_der();
        assert!(!der.as_ref().is_empty(), "DER 不能是空的");
        assert_eq!(der.as_ref()[0], 0x30, "DER 应当以 SEQUENCE 开头");
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der).expect("CA 的 DER 必须能装进根仓库");
        assert_eq!(roots.len(), 1);
    }
}
