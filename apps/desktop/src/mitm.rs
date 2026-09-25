//! MITM 通道的桌面运行态（P4 第四步）。
//!
//! # 三道闸门，缺一不可
//!
//! 1. **设置里开着**（`mitm.enabled`）且**名单非空** —— 用户明确选了要拆哪些域名；
//! 2. **根证书已经在系统钥匙串里**。这一条是[`core_settings`]存在的理由：
//!    没有它，引导规则会把用户的 HTTPS 送到一个**没人信的证书**上 ——
//!    那不是"过滤广告"，那是"把网站搞坏"。所以证书没装时，
//!    我们把 MITM 从**下发给核心的那份设置**里摘掉（fail-open：照常上网，不拆包）；
//! 3. **代理真的在监听**（[`MitmRuntime::is_running`]）。
//!
//! 三条里任何一条不满足，用户看到的是"MITM 没生效"，而不是"HTTPS 打不开"。
//!
//! # CA 的生命周期：只活在这次会话里（本版的边界）
//!
//! 每次启动生成一张新 CA，装进钥匙串的是它；退出时（或下次启动的过期会话回滚）
//! 会把它删掉。好处是**不留长期信任面**；代价是每次启动换一张根证书。
//! 让 CA 稳定需要持久化私钥，那是独立的一步，**本版没做** —— 所以界面上
//! 必须说清楚"每次启动都要重新信任一次"。
//!
//! # 为什么拆包域名和判定域名是两件事
//!
//! * `mitm.domains`（用户勾的）决定**哪些流量被拆**；
//! * 判定由 [`BlocklistDecider`] 拿到的 `block_hosts` 决定（来自意图引擎的拦截带）。
//!
//! 分开是对的：拆包是**有代价、有隐私含义**的动作，必须逐个域名由用户点；
//! 而"拆开之后拦不拦"应当与域名层的判决保持一致 —— 否则同一个域名在
//! 两层得到不同结论，用户没法理解，我们也没法解释。

use std::sync::Arc;

use xt_core::model::{AppSettings, MitmSettings};
use xt_mitm::{
    serve_with, BlocklistDecider, BodyRewriter, JsonStripRewriter, LocalCa, ProxyConfig,
    ProxyHandle, ProxyStatsSnapshot, TlsError,
};

/// 下发给核心的**有效设置**：根证书没被信任时，把 MITM 摘掉。
///
/// 这不是"少写一个开关"，而是这条链路上唯一的 fail-open 闸门：
/// 核心一旦带上 `mitm-steer` 规则，被 steer 的域名就只能由 MITM 接手 ——
/// 而 MITM 拿的是一张没人信的证书。所以宁可这次不拆包。
///
/// **不动 `settings.mitm` 本身**（用户的选择要留在设置里），只改即将下发的那一份。
pub fn core_settings(settings: &AppSettings, ca_trusted: bool) -> AppSettings {
    let mut effective = settings.clone();
    if !ca_trusted {
        effective.mitm.enabled = false;
    }
    effective
}

/// 本会话的 CA 现在是否被系统信任。
///
/// **读不出来就当"没信任"（fail-open）**：宁可这次不拆包，也不能拿一张
/// 可能没人信的证书去接用户的 HTTPS。少拆一次包的代价是"广告没拦住"，
/// 反过来则是"网站打不开"。
pub fn ca_is_trusted(fingerprint: Option<&str>) -> bool {
    let Some(fp) = fingerprint else {
        return false;
    };
    match xt_tun::macos::trust::is_trusted(fp) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "查询信任锚失败：按「未信任」处理（fail-open）");
            false
        }
    }
}

/// 从设置装配响应体裁剪器。没配就是 `None` —— 默认路径**连响应体都不看**。
///
/// 本版只支持一种口径：删掉 JSON 指针所指数组里、某个布尔字段为 `true` 的元素
/// （与 [`xt_core::model::MitmBodyStrip`] 一一对应）。
pub fn rewriter_for(settings: &MitmSettings) -> Option<Arc<dyn BodyRewriter>> {
    let strip = settings.body_strip.as_ref()?;
    if !strip.is_configured() {
        return None;
    }
    Some(Arc::new(JsonStripRewriter::new(
        strip.pointer.clone(),
        strip.field.clone(),
        serde_json::Value::Bool(true),
    )))
}

/// MITM 服务的运行态。`Default` = 没起、也没生成过 CA。
#[derive(Default)]
pub struct MitmRuntime {
    /// 本会话的 CA。**懒生成**：没启用 MITM 的用户不该在进程里多一张私钥。
    ca: Option<Arc<LocalCa>>,
    handle: Option<ProxyHandle>,
    /// 起当前这个代理用的"输入摘要"。设置变了但要重启才生效时，界面靠它说"待生效"。
    applied: Option<String>,
    /// **核心**最近一次启动时，配置里到底带没带引导规则。
    ///
    /// 引导规则要靠 `mitm-out` 出站与 `mitm-upstream` 入站，而这两样**没法热加**
    /// （`RoutingService` 只改规则表）—— 所以"证书刚装上"与"核心已经按它跑"
    /// 之间隔着一次重连。这个字段就是那个差值的唯一判据，跟 `intent.mark_applied`
    /// 同一个套路：**没记录过就不许说"已生效"**。
    core_steering: Option<bool>,
}

/// 给界面看的状态。字段名就是 TS 那边的字段名（契约测试盯着）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct MitmStatus {
    /// 用户在设置里开着。
    pub enabled: bool,
    /// 开着**且**名单非空（配置生成与判定的前提）。
    pub active: bool,
    /// 代理真的在监听。
    pub running: bool,
    pub listen_port: u16,
    pub upstream_port: u16,
    pub domains: Vec<String>,
    pub block_quic: bool,
    /// 本会话 CA 的 SHA-1 指纹（大写冒号分隔）—— 装/卸信任锚用的就是它。
    ///
    /// 没生成过 CA 时是 `None`（"还没启用过 MITM"这件事必须能被区分出来，
    /// 不能让界面显示一个假的指纹）。
    pub ca_fingerprint: Option<String>,
    /// 本会话 CA 的到期日（`YYYY-MM-DD`）。`None` = 还没生成过 CA。
    ///
    /// 根证书有效期必须**有界**（rcgen 默认等于永不过期）—— 到期日回传给界面，
    /// 让"该轮换 CA 了"这件事可见，而不是靠人去记。
    pub ca_expires_at: Option<String>,
    /// 当前这个代理的计数（没在跑时 `None`）。
    pub stats: Option<ProxyStatsSnapshot>,
    /// **一句话解释为什么没在跑**（`None` = 正常）。界面直接显示，不要自己猜。
    pub note: Option<String>,
    /// 起当前代理用的输入摘要（用于"设置改了但没重启"）。
    pub applied: Option<String>,
    /// 核心最近一次启动时的引导规则状态（`None` = 没记录过/核心没在跑）。
    pub core_steering: Option<bool>,
    /// 引导规则要重连核心才生效（证书刚装/刚卸、或名单刚改）。
    pub core_restart_required: bool,
}

impl MitmRuntime {
    /// 一次启动的输入摘要：端口、名单、判定名单都进去。
    ///
    /// 用它判断"当前跑着的代理是不是这份设置的产物" —— 但**不自动重启**：
    /// 重启会打断所有连接，和核心规则一样必须由用户显式触发（`mitm_apply`）。
    fn digest(settings: &MitmSettings, block_hosts: &[String], rewriter: bool) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}",
            settings.listen_port,
            settings.upstream_port,
            settings.domains.join(","),
            settings.block_quic,
            block_hosts.join(","),
            rewriter
        )
    }

    /// 生成（或取回）本会话的 CA。
    ///
    /// 失败**不缓存**：下一次调用会再试一次，而不是把一个"永远失败"记成状态。
    ///
    /// 命中缓存走**早返回**，所以「生成 → 放进槽位 → 返回」不需要
    /// `self.ca.as_ref().expect("刚放进去了")` 来收尾：那个 `expect` 逻辑上
    /// 确实不可达，但只要它存在，「这里不可能为空」就是**靠人记住**的不变量；
    /// release profile 是 `panic = "abort"`，将来改动打破它时用户看到的是 SIGABRT。
    pub fn ca(&mut self) -> Result<Arc<LocalCa>, String> {
        if let Some(ca) = &self.ca {
            return Ok(ca.clone());
        }
        let ca = Arc::new(LocalCa::generate().map_err(|e| format!("生成本地根证书失败：{e}"))?);
        self.ca = Some(ca.clone());
        Ok(ca)
    }

    /// 给 helper 的 PEM（**不含私钥** —— helper 会拒收含私钥的 PEM，这是安全边界）。
    pub fn ca_pem(&mut self) -> Result<String, String> {
        Ok(self.ca()?.cert_pem().to_string())
    }

    /// CA 的 SHA-1 指纹（`security` 用这个值定位钥匙串条目）。
    pub fn ca_fingerprint(&mut self) -> Result<String, String> {
        let ca = self.ca()?;
        Ok(xt_tun::macos::trust::sha1_fingerprint(
            ca.cert_der().as_ref(),
        ))
    }

    pub fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// 核心启动时调用：记下这次下发的配置里有没有引导规则。
    pub fn mark_core_steering(&mut self, steered: bool) {
        self.core_steering = Some(steered);
    }

    /// 核心停下时调用：没有核心，"核心那边生不生效"这个问题就不该有答案。
    pub fn clear_core_steering(&mut self) {
        self.core_steering = None;
    }

    /// 本会话 CA 的指纹，**不生成**（没启用过 MITM 就不该有私钥在内存里）。
    pub fn existing_fingerprint(&self) -> Option<String> {
        self.ca
            .as_ref()
            .map(|ca| xt_tun::macos::trust::sha1_fingerprint(ca.cert_der().as_ref()))
    }

    /// 起代理。已经用同一份输入在跑时是空操作（幂等），不会白白换个监听端口。
    pub fn start(
        &mut self,
        settings: &MitmSettings,
        block_hosts: Vec<String>,
        rewriter: Option<Arc<dyn BodyRewriter>>,
    ) -> Result<(), String> {
        let cfg = proxy_config(settings).map_err(|e| e.to_string())?;
        let key = Self::digest(settings, &block_hosts, rewriter.is_some());
        if self.handle.is_some() && self.applied.as_deref() == Some(key.as_str()) {
            return Ok(());
        }
        // 换设置 ⇒ 先停旧的：两个实例抢同一个端口只会得到"启动失败"，
        // 而那个错误信息会误导用户以为端口被别的程序占了。
        self.stop();
        let ca = self.ca()?;
        let decider = Arc::new(BlocklistDecider::new(block_hosts));
        let handle = serve_with(cfg, ca, decider, rewriter)
            .map_err(|e| format!("启动 MITM 代理失败：{e}"))?;
        tracing::info!(
            listen = %handle.listen,
            domains = settings.domains.len(),
            "MITM 代理已启动"
        );
        self.handle = Some(handle);
        self.applied = Some(key);
        Ok(())
    }

    /// 停代理。幂等；**留下的 CA 不动**（卸信任锚是单独的、需要 helper 的动作）。
    pub fn stop(&mut self) {
        if self.handle.take().is_some() {
            tracing::info!("MITM 代理已停止");
        }
        self.applied = None;
    }

    /// 按当前设置求出状态。取 `ca_trusted` 由调用方给（它要跑 `security(1)` 查询，
    /// 不该在这个纯读取路径里做 IO）。
    pub fn status(&self, settings: &MitmSettings, ca_trusted: bool) -> MitmStatus {
        let effective = settings.is_active() && ca_trusted;
        let active = settings.is_active();
        let running = self.handle.is_some();
        let note = if !settings.enabled {
            Some("MITM 没开启".to_string())
        } else if settings.domains.is_empty() {
            Some("名单为空：一个域名都不会被拆包".to_string())
        } else if !ca_trusted {
            // 最要紧的一条：证书没装时**核心不会收到引导规则**（见 `core_settings`）。
            Some("根证书还没装进系统钥匙串：引导规则不会下发给核心，HTTPS 照常直连".to_string())
        } else if !running {
            Some("根证书已信任，但代理没在跑（点「应用」启动）".to_string())
        } else {
            None
        };
        // "证书现在装好了、但核心还按旧配置在跑" —— 这是最容易让用户困惑的一步，
        // 必须明说，而不是让他自己去猜为什么还是没生效。
        let core_restart_required = match self.core_steering {
            Some(applied) => applied != effective,
            None => false,
        };
        MitmStatus {
            enabled: settings.enabled,
            active,
            running,
            listen_port: settings.listen_port,
            upstream_port: settings.upstream_port,
            domains: settings.domains.clone(),
            block_quic: settings.block_quic,
            ca_fingerprint: self
                .ca
                .as_ref()
                .map(|ca| xt_tun::macos::trust::sha1_fingerprint(ca.cert_der().as_ref())),
            ca_expires_at: self.ca.as_ref().map(|ca| ca.expiry_ymd()),
            stats: self.handle.as_ref().map(|h| h.stats()),
            note: if core_restart_required {
                Some(match note {
                    // 已经有原因（比如证书没装）时，只补上"核心那边"的这句。
                    Some(n) => format!("{n}；另外：核心要重连一次才会带上引导规则"),
                    None => "引导规则要重连一次核心才会生效".to_string(),
                })
            } else {
                note
            },
            applied: self.applied.clone(),
            core_steering: self.core_steering,
            core_restart_required,
        }
    }
}

/// 校验并构造代理配置。
///
/// 抽出来是为了让"[自环](https://example.invalid) 的三种写法"都能被单测钉住：
/// 端口为 0、监听与回连同端口、名单为空。
fn proxy_config(settings: &MitmSettings) -> Result<ProxyConfig, TlsError> {
    if !settings.enabled {
        return Err(TlsError::Config("MITM 没开启".to_string()));
    }
    if settings.domains.is_empty() {
        return Err(TlsError::Config(
            "MITM 名单为空：不拆任何域名（这是有意的，不是错误配置）".to_string(),
        ));
    }
    if settings.listen_port == 0 || settings.upstream_port == 0 {
        return Err(TlsError::Config("MITM 端口不能是 0".to_string()));
    }
    if settings.listen_port == settings.upstream_port {
        return Err(TlsError::Config(format!(
            "MITM 的监听端口与回连端口相同（{}）—— 那会自环",
            settings.listen_port
        )));
    }
    Ok(ProxyConfig {
        // **只听回环**：MITM 绝不对局域网暴露（它就装在用户机器上）。
        listen: format!("127.0.0.1:{}", settings.listen_port)
            .parse()
            .map_err(|e| TlsError::Config(format!("监听地址不合法：{e}")))?,
        upstream_socks: format!("127.0.0.1:{}", settings.upstream_port)
            .parse()
            .map_err(|e| TlsError::Config(format!("回连地址不合法：{e}")))?,
        io_timeout: std::time::Duration::from_secs(20),
        max_connections: 256,
        // 生产走 443：`freedom.redirect` 不传递原始目标（数据面的硬约束）。
        assumed_port: 443,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xt_core::model::RoutingPreset;

    fn settings_with_mitm(domains: &[&str]) -> AppSettings {
        let mut s = AppSettings {
            routing_preset: RoutingPreset::BypassMainland,
            ..Default::default()
        };
        s.mitm.enabled = true;
        s.mitm.domains = domains.iter().map(|d| d.to_string()).collect();
        s
    }

    /// **这条是闸门 2 的判别性测试**：证书没信任时，引导规则**绝不能**进核心配置。
    ///
    /// 没有它，一个"照常生成 steer 规则"的实现也能让所有其它测试通过 ——
    /// 而用户看到的是"开了 MITM 之后那几个网站的 HTTPS 全打不开"。
    #[test]
    fn an_untrusted_ca_keeps_the_steering_rules_out_of_the_core_config() {
        let s = settings_with_mitm(&["ads.example"]);
        let trusted = core_settings(&s, true);
        let untrusted = core_settings(&s, false);

        let rules_trusted = xt_core::xray::merge_rules_with_intent(&trusted, &[], &[]);
        let rules_untrusted = xt_core::xray::merge_rules_with_intent(&untrusted, &[], &[]);
        assert!(
            rules_trusted.iter().any(|r| r.id.starts_with("mitm-steer")),
            "证书已信任 ⇒ 引导规则必须在配置里：{:?}",
            rules_trusted.iter().map(|r| &r.id).collect::<Vec<_>>()
        );
        assert!(
            !rules_untrusted
                .iter()
                .any(|r| r.id.starts_with("mitm-steer")),
            "证书没信任 ⇒ **不许**有引导规则（否则 HTTPS 会被拆到一个没人信的证书上）"
        );

        // 而且用户的设置本身不能被改掉：只是"这一份不下发"。
        assert!(s.mitm.enabled, "用户的选择必须留在设置里");
        assert!(!untrusted.mitm.enabled, "下发的那一份才被摘掉");
    }

    #[test]
    fn a_trusted_ca_changes_nothing_else_in_the_settings() {
        let s = settings_with_mitm(&["ads.example"]);
        let effective = core_settings(&s, true);
        assert_eq!(effective.mitm.domains, s.mitm.domains);
        assert!(effective.mitm.enabled);
    }

    #[test]
    fn self_loop_and_empty_list_are_refused_with_readable_reasons() {
        let mut s = MitmSettings {
            enabled: true,
            domains: vec![],
            ..Default::default()
        };
        let e = proxy_config(&s).unwrap_err().to_string();
        assert!(e.contains("名单为空"), "{e}");

        s.domains = vec!["ads.example".into()];
        s.listen_port = 0;
        let e = proxy_config(&s).unwrap_err().to_string();
        assert!(e.contains("不能是 0"), "{e}");

        s.listen_port = 10810;
        s.upstream_port = 10810;
        let e = proxy_config(&s).unwrap_err().to_string();
        assert!(e.contains("自环"), "{e}");

        s.upstream_port = 10811;
        s.enabled = false;
        let e = proxy_config(&s).unwrap_err().to_string();
        assert!(e.contains("没开启"), "{e}");
    }

    /// CA 的 PEM 必须**不含私钥**（helper 会拒收），指纹必须过 helper 的校验。
    #[test]
    fn the_ca_pem_has_no_private_key_and_the_fingerprint_is_well_formed() {
        let mut rt = MitmRuntime::default();
        let pem = rt.ca_pem().unwrap();
        assert!(pem.contains("BEGIN CERTIFICATE"), "PEM 里必须有证书块");
        assert!(
            !pem.contains("PRIVATE KEY"),
            "私钥绝不能离开进程（helper 会拒收，这也是安全边界）"
        );
        let fp = rt.ca_fingerprint().unwrap();
        xt_tun::macos::trust::validate_fingerprint(&fp).expect("指纹必须能过 helper 的校验");
        assert_eq!(xt_tun::macos::trust::normalize_fingerprint(&fp).len(), 40);
        // 同一个 runtime 两次取必须一样（否则装进去的与查到的是两张证书）。
        assert_eq!(rt.ca_fingerprint().unwrap(), fp);
    }

    /// CA 只生成一次并被复用：删掉那句 `expect("刚放进去了")` 之后，
    /// 「生成 → 放进槽位 → 返回」仍必须是一个原子动作（同一个 `Arc`）。
    ///
    /// 判别性：如果有人把 `ca()` 改回「每次生成一张新 CA」，两次调用返回的
    /// 指针不同 ⇒ 这条红（已装进钥匙串的信任锚会对不上界面上显示的指纹）。
    #[test]
    fn ca_is_generated_once_and_reused() {
        let mut rt = MitmRuntime::default();
        let first = rt.ca().expect("第一次必须能生成 CA");
        let second = rt.ca().expect("第二次必须走缓存，不许重新生成");
        assert!(
            Arc::ptr_eq(&first, &second),
            "两次 `ca()` 必须是**同一个** CA 实例；重新生成会让已装的信任锚对不上"
        );
    }

    /// 起 → 真的在监听 → 停 → 真的不监听了。
    ///
    /// 这条是"代理真的起来了"的最低证据：不连一次端口，`is_running()` 只是个 bool。
    #[test]
    fn start_binds_a_real_listener_and_stop_releases_it() {
        let port = free_port();
        let upstream = free_port();
        let mut rt = MitmRuntime::default();
        let s = MitmSettings {
            enabled: true,
            domains: vec!["ads.example".into()],
            listen_port: port,
            upstream_port: upstream,
            block_quic: false,
            body_strip: None,
        };
        rt.start(&s, vec!["ads.example".into()], None)
            .expect("起代理");
        assert!(rt.is_running());
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "起完之后端口必须是通的"
        );
        // 幂等：同一份输入再起一次不该报错、也不该换端口。
        rt.start(&s, vec!["ads.example".into()], None)
            .expect("重复启动应当幂等");
        assert!(rt.is_running());

        let status = rt.status(&s, true);
        assert!(status.running && status.active);
        assert!(
            status.note.is_none(),
            "一切正常时不该有解释文案：{:?}",
            status.note
        );
        assert!(status.stats.is_some(), "跑着就必须有计数");
        assert!(status.ca_fingerprint.is_some(), "起过代理就一定有 CA");

        rt.stop();
        assert!(!rt.is_running(), "停完必须不在跑");
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
            "停完端口必须已经释放"
        );
    }

    /// 状态里的"为什么没在跑"必须是**四种不同**的原因，而不是一句笼统的话。
    #[test]
    fn the_status_explains_exactly_why_it_is_not_running() {
        let rt = MitmRuntime::default();
        let off = MitmSettings {
            enabled: false,
            domains: vec!["a.test".into()],
            ..Default::default()
        };
        assert!(rt.status(&off, true).note.unwrap().contains("没开启"));

        let empty = MitmSettings {
            enabled: true,
            domains: vec![],
            ..Default::default()
        };
        let st = rt.status(&empty, true);
        assert!(!st.active);
        assert!(st.note.unwrap().contains("名单为空"));

        let on = MitmSettings {
            enabled: true,
            domains: vec!["a.test".into()],
            ..Default::default()
        };
        let st = rt.status(&on, false);
        assert!(st.active);
        assert!(
            st.note.unwrap().contains("根证书还没装"),
            "证书没装时必须说这一条（它决定了核心拿不到引导规则）"
        );

        let st = rt.status(&on, true);
        assert!(st.note.unwrap().contains("代理没在跑"));
        // 没生成过 CA 时不许编一个指纹出来。
        assert!(st.ca_fingerprint.is_none());
    }

    fn free_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
        l.local_addr().unwrap().port()
    }
}
