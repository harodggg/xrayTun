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

use std::path::{Path, PathBuf};

use xt_core::model::{AppSettings, MitmSettings};
use xt_mitm::{
    serve_with_observer, BlocklistDecider, BodyRewriter, JsonStripRewriter, LocalCa, ProxyConfig,
    ProxyHandle, ProxyStatsSnapshot, TlsError,
};

use crate::observe::{
    effective_markers, observe_config_for, report_path_in, ObserveArchive, ObserveLedger,
    ObserveReport, RecordingObserver,
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
    /// 本会话的观察汇总（按域名的标记词命中结论）。
    ///
    /// **不清空**：代理停了之后"刚才到底看到了什么"仍然是已发生的事实，
    /// 用户点「应用（起/停代理）」停下后应当还能看见结论；下次启动会换一份新的。
    observe: Arc<ObserveLedger>,
    /// 当前代理实际用的观察者（诊断/测试用）。`stop` 之后为 `None`。
    observer: Option<Arc<RecordingObserver>>,
    /// 观察留档目标（数据目录下的固定文件）。`None` = 不落盘（测试 / 未配置）。
    ///
    /// 由命令层在 `mitm_apply` 时设置；`Default` 是 `None`，这样单测不会
    /// 意外往真实数据目录里写东西（判据①要的就是"默认不产生文件"）。
    report_path: Option<PathBuf>,
    /// 留档器（`stop` 之后**保留**：导出/清空要能拿到会话口径）。
    archive: Option<Arc<ObserveArchive>>,
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
    /// **只观察、不改写**的结论（按域名的标记词命中汇总；默认关）。
    ///
    /// 它是观察配置与已采数据的合并视图；字段含义与隐私口径见
    /// [`crate::observe::ObserveReport`]。
    pub observe: ObserveReport,
}

impl MitmRuntime {
    /// 一次启动的输入摘要：端口、名单、判定名单都进去。
    ///
    /// 用它判断"当前跑着的代理是不是这份设置的产物" —— 但**不自动重启**：
    /// 重启会打断所有连接，和核心规则一样必须由用户显式触发（`mitm_apply`）。
    fn digest(settings: &MitmSettings, block_hosts: &[String], rewriter: bool) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            settings.listen_port,
            settings.upstream_port,
            settings.domains.join(","),
            settings.block_quic,
            block_hosts.join(","),
            rewriter,
            // 观察配置也要进摘要：改了"看哪些域名/落不落盘/用哪份词表"之后，
            // 用户点「应用」必须真的换掉代理里的观察者（否则界面显示的和跑的不是一回事）。
            settings.observe.enabled,
            settings.observe.hosts.join(","),
            settings
                .observe
                .capture_body_dir
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            // 词表进摘要用**生效值**：`None`（默认表）与 `Some(默认表)` 是同一份口径，
            // 不该因为写法不同就重启一次代理。
            effective_markers(settings).join(",")
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

    /// 设观察留档的**数据目录根**（命令层在 `mitm_apply` 时调用）。
    ///
    /// 留档路径固定为根下的 [`report_path_in`] —— 不让用户选：用户要选路径的那个
    /// 动作是「导出」，不是留档。`Default` 是 `None`：单测不会意外往真实数据目录写。
    pub fn set_report_path_in(&mut self, root: &Path) {
        self.report_path = Some(report_path_in(root));
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
        // 每次启动换一份新的观察汇总、观察者与留档器：旧数据属于上一份配置，混在一起
        // 会让"这个域名到底采到没有"说不清。
        let ledger = Arc::new(ObserveLedger::default());
        let archive = Arc::new(ObserveArchive::new(settings, self.report_path.clone()));
        let observer: Arc<RecordingObserver> = Arc::new(
            RecordingObserver::new(&observe_config_for(settings), ledger.clone())
                .with_archive(archive.clone()),
        );
        let handle = serve_with_observer(cfg, ca, decider, rewriter, observer.clone())
            .map_err(|e| format!("启动 MITM 代理失败：{e}"))?;
        tracing::info!(
            listen = %handle.listen,
            domains = settings.domains.len(),
            observe_enabled = settings.observe.enabled,
            observe_hosts = settings.observe.hosts.len(),
            "MITM 代理已启动"
        );
        self.handle = Some(handle);
        self.applied = Some(key);
        self.observe = ledger;
        self.archive = Some(archive);
        self.observer = Some(observer);
        Ok(())
    }

    /// 停代理。幂等；**留下的 CA 不动**（卸信任锚是单独的、需要 helper 的动作）。
    ///
    /// 观察汇总**不清空**（见字段注释）；只把"当前代理用的观察者"摘掉。
    /// 停的时候把**最后一份**摘要强制落盘（带结束时间）—— 零摘要时**不产生文件**。
    /// 写失败不静默：记进留档器的 `last_error`，`status().observe.note` 会显示它。
    pub fn stop(&mut self) {
        if self.handle.take().is_some() {
            tracing::info!("MITM 代理已停止");
        }
        if !self.observe.is_empty() {
            self.observe.mark_stopped();
            if let Some(archive) = &self.archive {
                if let Err(e) = archive.write_now(&self.observe) {
                    tracing::warn!(error = %e, "停止时写观察留档失败");
                }
            }
        }
        self.applied = None;
        self.observer = None;
    }

    /// 导出当前观察结论到用户给的**绝对路径**；返回写入的路径。
    ///
    /// 导出与数据目录留档共用 [`ObserveArchive::document`]，因此**内容同源**。
    /// 失败一律给出可读原因（相对路径 / 没有摘要 / 没启动过 / 写文件失败）。
    pub fn export_observe(&self, path: &str) -> Result<String, String> {
        let target = PathBuf::from(path.trim());
        if !target.is_absolute() {
            return Err(format!(
                "导出路径必须是绝对路径：{path} —— 相对路径会落到当前工作目录（可能是仓库）"
            ));
        }
        if self.observe.is_empty() {
            return Err("还没有采到任何摘要：没有东西可以导出".to_string());
        }
        let archive = self.archive.as_ref().ok_or_else(|| {
            "观察没有启动过：先点「应用」把 MITM 跑起来，采到摘要后再导出".to_string()
        })?;
        let doc = archive.document(&self.observe, xt_core::util::now_unix());
        crate::observe::write_document(&target, &doc)
            .map_err(|e| format!("导出观察结论失败：{e}"))?;
        Ok(target.display().to_string())
    }

    /// 清空**内存**里的观察结论（就地清空，跑着的观察者看得到）。
    ///
    /// 数据目录留档文件由命令层按 root 删除（那里才有数据目录）。
    pub fn clear_observe(&mut self) {
        self.observe.clear();
    }

    /// 留档器最近一次失败的可读原因（没有失败时 `None`）——不许静默。
    pub fn observe_persist_error(&self) -> Option<String> {
        self.archive.as_ref().and_then(|a| a.last_error())
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
            // 观察报告是"配置 + 已采数据"的合并视图：即使代理没在跑，
            // 也要能回答"观察开没开、名单是什么、有没有采到过"。
            observe: {
                let mut report = self.observe.report(settings);
                // 留档失败**不许静默**：并进 note，让界面上那条 banner 显示出来
                // （用户会以为自己真的在留档，实际一条都没写下去）。
                if let Some(err) = self.observe_persist_error() {
                    report.note = Some(match report.note {
                        Some(n) => format!("{n}；另外：{err}"),
                        None => err,
                    });
                }
                report
            },
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
    use xt_mitm::Observer;

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
            observe: Default::default(),
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

    /// **task-24 第 1 项（三闸门组合判据）**：闸门①（名单非空）与②（CA 信任）都过之后，
    /// 「核心那边到底生没生效」只剩 `core_steering` 这一个判据 —— 三种取值都必须钉住：
    ///
    /// * `Some(true)`  + 全绿 ⇒ 不需要重连、没有 note；
    /// * `Some(false)` + CA 已信任 ⇒ **需要重连**（证书刚装上，核心还是旧配置），note 要明说；
    /// * `None`（没记录过）⇒ **不许声称需要重连**，而且 `core_steering` 必须原样保留为 `None`
    ///   —— null 是独立一态（"没记录过"），不许被"简化"成 bool。
    ///
    /// 判别性：把 `core_restart_required` 写死 `false` ⇒ 中间的断言红；
    /// 把 `None` 压成 `Some(false)` 或 `false` ⇒ 最后两条红。
    #[test]
    fn three_gates_matrix_pins_core_restart_and_the_null_tristate() {
        let mut s = settings_with_mitm(&["ads.example"]);
        // 两个端口必须不同（自环配置会被 `proxy_config` 拒绝）；随机端口偶尔会撞。
        let listen = free_port();
        let mut upstream = free_port();
        while upstream == listen {
            upstream = free_port();
        }
        s.mitm.listen_port = listen;
        s.mitm.upstream_port = upstream;

        // (c) 闸门①②都过，但核心上次启动时**没带**引导规则 ⇒ 要重连一次。
        let mut rt = MitmRuntime::default();
        rt.mark_core_steering(false);
        let st = rt.status(&s.mitm, true);
        assert!(st.active, "名单非空 + 开着 ⇒ 闸门①过");
        assert_eq!(st.core_steering, Some(false), "记录必须如实保留");
        assert!(st.core_restart_required, "(c) 核心那次没带引导规则 ⇒ 必须说要重连");
        assert!(
            st.note.as_deref().unwrap_or_default().contains("重连"),
            "(c) 必须用一句人话说明要重连：{:?}",
            st.note
        );

        // (d) 全绿：代理真的在跑 + 核心那次**带了**引导规则。
        let mut rt = MitmRuntime::default();
        rt.start(&s.mitm, vec!["ads.example".into()], None)
            .expect("起代理");
        rt.mark_core_steering(true);
        let st = rt.status(&s.mitm, true);
        assert!(st.active && st.running, "(d) 全绿：active 且 running");
        assert_eq!(st.core_steering, Some(true));
        assert!(!st.core_restart_required, "(d) 核心已按它跑 ⇒ 不需要重连");
        assert!(st.note.is_none(), "(d) 全绿不该有 note：{:?}", st.note);
        rt.stop();

        // (e) 没记录过：不声称，且 null 不许被压成 bool。
        let rt = MitmRuntime::default();
        let st = rt.status(&s.mitm, true);
        assert_eq!(st.core_steering, None, "null 是独立一态：'没记录过'");
        assert!(
            !st.core_restart_required,
            "'没记录过'不许被当成'需要重连'（那是在编）"
        );
    }

    /// **闸门②在两层必须是同一判据**：状态层（`active && ca_trusted`）与
    /// 配置层（`core_settings(...).mitm.is_active()`）不许漂移。
    ///
    /// 判别性：任何只改一层的"收敛"都会让这条红 —— 它钉的是两处推导的一致性，
    /// 而不是某一个具体取值。
    #[test]
    fn ca_gate_agrees_between_status_and_core_settings() {
        for (enabled, domains, trusted) in [
            (true, vec![], true),
            (true, vec!["ads.example"], false),
            (true, vec!["ads.example"], true),
            (false, vec!["ads.example"], true),
        ] {
            let mut s = settings_with_mitm(&domains);
            s.mitm.enabled = enabled;
            let rt = MitmRuntime::default();
            let st = rt.status(&s.mitm, trusted);
            let status_layer = st.active && trusted;
            let config_layer = core_settings(&s, trusted).mitm.is_active();
            assert_eq!(
                status_layer, config_layer,
                "闸门②两层漂移：enabled={enabled} domains={domains:?} trusted={trusted}"
            );
        }
    }

    /// **逐字段 golden（比 721325d 的组合断言更强）**：`MitmStatus` 是给界面的契约，
    /// 收敛三道闸门时**任何一个字段的名字或取值漂移都必须红**。
    ///
    /// 五种组合覆盖 `note` 的全部四个分支 + `core_steering` 的三态：
    /// ① 关着 ② 开着但名单空 ③ 开着+名单非空+CA 未信任
    /// ④ 闸门①②都过、但没记录过核心状态（`core_steering = null` ⇒ 不声称要重连）
    /// ⑤ 闸门①②都过 + 核心上次**没带**引导规则（⇒ 要重连，note 追加那句）
    ///
    /// 为什么不起真代理：`stats` 会带动态计数器/监听端口，golden 就不稳定；
    /// `running=true` 那一格由 `three_gates_matrix_pins_core_restart_and_the_null_tristate` 覆盖。
    ///
    /// 判别性（本次真做过突变实验）：把 `core_restart_required` 写死 `false`
    /// ⇒ ⑤ 红；把 `None` 压成 `Some(false)` ⇒ ④ 的 `core_steering` 红；
    /// 把闸门②（CA 信任）从 active 里去掉 ⇒ ③ 的 note 红。
    #[test]
    fn mitm_status_is_pinned_field_by_field_across_gate_combinations() {
        fn status_json(
            enabled: bool,
            domains: &[&str],
            trusted: bool,
            steering: Option<bool>,
        ) -> serde_json::Value {
            let s = MitmSettings {
                enabled,
                domains: domains.iter().map(|d| d.to_string()).collect(),
                listen_port: 18080,
                upstream_port: 18081,
                ..Default::default()
            };
            let mut rt = MitmRuntime::default();
            if let Some(v) = steering {
                rt.mark_core_steering(v);
            }
            serde_json::to_value(rt.status(&s, trusted)).expect("MitmStatus 必须能序列化")
        }

        let expect = |enabled: bool,
                      active: bool,
                      domains: &[&str],
                      note: Option<&str>,
                      steering: Option<bool>,
                      restart: bool| {
            serde_json::json!({
                "enabled": enabled,
                "active": active,
                "running": false,
                "listen_port": 18080,
                "upstream_port": 18081,
                "domains": domains,
                "block_quic": false,
                "ca_fingerprint": null,
                "ca_expires_at": null,
                "stats": null,
                "note": note,
                "applied": null,
                "core_steering": steering,
                "core_restart_required": restart,
                // 观察报告字段名也是给界面的契约：这里逐字段钉住（默认关 ⇒ 零摘要）。
                "observe": {
                    "enabled": false,
                    "configured_hosts": [],
                    "markers": xt_mitm::default_markers(),
                    "marker_counting": true,
                    "exchanges": 0,
                    "marker_total": 0,
                    "hosts": [],
                    "capture_body_dir": null,
                    "note": null,
                },
            })
        };

        // ① 关着
        assert_eq!(
            status_json(false, &["ads.example"], true, None),
            expect(false, false, &["ads.example"], Some("MITM 没开启"), None, false),
            "① 关着：enabled=false、note=没开启、不声称要重连"
        );
        // ② 开着但名单空
        assert_eq!(
            status_json(true, &[], true, None),
            expect(true, false, &[], Some("名单为空：一个域名都不会被拆包"), None, false),
            "② 名单空：active=false（闸门①没过）"
        );
        // ③ 开着+名单非空+CA 未信任
        assert_eq!(
            status_json(true, &["ads.example"], false, None),
            expect(
                true,
                true,
                &["ads.example"],
                Some("根证书还没装进系统钥匙串：引导规则不会下发给核心，HTTPS 照常直连"),
                None,
                false
            ),
            "③ 未信任：闸门②没过 ⇒ note 必须指向证书（引导规则不下发）"
        );
        // ④ 闸门①②都过 + core_steering = null（没记录过）
        assert_eq!(
            status_json(true, &["ads.example"], true, None),
            expect(
                true,
                true,
                &["ads.example"],
                Some("根证书已信任，但代理没在跑（点「应用」启动）"),
                None,
                false
            ),
            "④ null 是独立一态：不声称需要重连"
        );
        // ⑤ 闸门①②都过 + 核心上次没带引导规则
        assert_eq!(
            status_json(true, &["ads.example"], true, Some(false)),
            expect(
                true,
                true,
                &["ads.example"],
                Some(
                    "根证书已信任，但代理没在跑（点「应用」启动）\
                     ；另外：核心要重连一次才会带上引导规则"
                ),
                Some(false),
                true
            ),
            "⑤ 核心那次没带 ⇒ core_restart_required=true 且 note 追加那句"
        );
    }

    fn free_port() -> u16 {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("拿空闲端口");
        l.local_addr().unwrap().port()
    }

    // -----------------------------------------------------------------------
    // task-「观察接进 App」：把 ObserveConfig / Observer 真的交给代理
    // -----------------------------------------------------------------------

    /// 起代理用的两个**互不相同**的空闲端口（同端口会被 `proxy_config` 判自环）。
    ///
    /// 观察配置**保持默认（关）**：要不要观察由每条测试自己显式说，
    /// 默认值本身就是判据①要钉的东西。
    fn mitm_settings_with_ports() -> MitmSettings {
        let listen = free_port();
        let mut upstream = free_port();
        while upstream == listen {
            upstream = free_port();
        }
        MitmSettings {
            enabled: true,
            domains: vec!["news.example".into()],
            listen_port: listen,
            upstream_port: upstream,
            ..Default::default()
        }
    }

    fn observed_record(host: &str, body: &[u8]) -> xt_mitm::ExchangeRecord {
        let meta = xt_mitm::ExchangeMeta {
            host: host.into(),
            method: "GET".into(),
            path: "/api/timeline".into(),
            status: 200,
            status_line: "HTTP/1.1 200 OK".into(),
            content_type: Some("application/json".into()),
            body_bytes: body.len(),
            request_body_bytes: 0,
        };
        xt_mitm::summarize(&meta, body, &xt_mitm::default_markers())
    }

    /// **接线判据**：`start` 之后，观察者必须真的被装进运行态，而且它采到的摘要
    /// 必须出现在 `status().observe` 里（否则界面看到的永远是空的）。
    ///
    /// 判别性：把 `start` 里的 `serve_with_observer(...)` 换回 `serve_with(...)`，
    /// 或者忘了 `self.observer = Some(...)` / `self.observe = ledger` ⇒ 这条红。
    #[test]
    fn start_wires_the_observer_and_its_summaries_reach_the_status() {
        let mut s = mitm_settings_with_ports();
        s.observe.enabled = true;
        s.observe.hosts = vec!["news.example".into()];
        let mut rt = MitmRuntime::default();
        rt.start(&s, vec![], None).expect("起代理");

        let ob = rt.observer.clone().expect("起代理必须装上观察者");
        assert!(ob.observes("news.example"), "名单里的域名必须被观察");
        assert!(ob.observes("cdn.news.example"), "子域命中（与库层同一套规则）");
        assert!(!ob.observes("tracker.example"), "名单外不看");

        let body = br#"{"a":{"promoted":true},"b":{"promoted":false}}"#;
        let rec = observed_record("news.example", body);
        assert_eq!(rec.marker_total, 2, "promoted 出现两次");
        ob.observe(&rec, body);

        let st = rt.status(&s, true);
        assert!(st.observe.enabled);
        assert_eq!(st.observe.exchanges, 1, "摘要必须真的进了报告");
        assert_eq!(st.observe.hosts.len(), 1);
        assert_eq!(st.observe.hosts[0].host, "news.example");
        assert_eq!(st.observe.hosts[0].marker_total, 2);
        assert_eq!(
            st.observe.hosts[0]
                .markers
                .iter()
                .find(|m| m.marker == "promoted")
                .map(|m| m.count),
            Some(2),
            "promoted × 2"
        );
        rt.stop();
    }

    /// **判据（负例）**：默认设置（观察关）起代理 ⇒ 一条摘要都不写。
    ///
    /// 判别性：把 `ObserveSettings::default()` 的 `enabled` 改成 `true` ⇒ 红。
    #[test]
    fn a_default_start_observes_nothing() {
        let mut s = mitm_settings_with_ports();
        s.observe = xt_core::model::ObserveSettings::default();
        let mut rt = MitmRuntime::default();
        rt.start(&s, vec![], None).expect("起代理");

        let ob = rt.observer.clone().expect("观察者仍在（只是它说「不看」）");
        assert!(!ob.observes("news.example"), "默认关：一个域名都不看");
        let body = br#"{"promoted":true}"#;
        ob.observe(&observed_record("news.example", body), body);

        let st = rt.status(&s, true);
        assert!(!st.observe.enabled);
        assert_eq!(st.observe.exchanges, 0, "默认关必须是零摘要");
        assert!(st.observe.hosts.is_empty());
        assert_eq!(st.observe.marker_total, 0);
        rt.stop();
    }

    /// 观察配置变了，`digest` 必须跟着变 —— 否则点「应用」是空操作，
    /// 界面显示的新名单和代理里真正跑的不是一回事。
    #[test]
    fn changing_the_observe_config_changes_the_applied_digest() {
        let s = mitm_settings_with_ports();
        let a = MitmRuntime::digest(&s, &[], false);

        let mut on = s.clone();
        on.observe.enabled = true;
        let b = MitmRuntime::digest(&on, &[], false);
        assert_ne!(a, b, "开关进摘要");

        let mut hosts = s.clone();
        hosts.observe.hosts = vec!["news.example".into()];
        let c = MitmRuntime::digest(&hosts, &[], false);
        assert_ne!(a, c, "名单进摘要");
        assert_ne!(b, c);

        let mut dir = s.clone();
        dir.observe.capture_body_dir = Some(std::path::PathBuf::from("/tmp/xraytun-observe"));
        let d = MitmRuntime::digest(&dir, &[], false);
        assert_ne!(a, d, "落盘目录进摘要");

        // 词表必须进摘要：否则用户改完词表点「应用」是个空操作，
        // 界面显示新词表、代理还用旧词表计数。
        let mut markers = s.clone();
        markers.observe.markers = Some(vec!["sponsored".into()]);
        let e = MitmRuntime::digest(&markers, &[], false);
        assert_ne!(a, e, "词表进摘要");

        // 但 `None`（缺省 = 默认表）与 `Some(默认表)` 是**同一份**口径：不该白白重启。
        let mut same = s.clone();
        same.observe.markers = Some(xt_mitm::default_markers());
        assert_eq!(
            MitmRuntime::digest(&same, &[], false),
            a,
            "生效词表相同 ⇒ 摘要相同（写法不同不该触发重启）"
        );
    }

    // -----------------------------------------------------------------------
    // 「观察结论可持久化 / 可导出」在运行态这一层的接线
    // -----------------------------------------------------------------------

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("xt-mitm-observe-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时数据目录");
        dir
    }

    /// **判据①（落盘面）**：默认关起代理 → 停 → 数据目录里**一个文件都没有**。
    ///
    /// 判别性：把 `ObserveArchive::write` 里那句"零摘要直接返回"删掉 ⇒ 这条红。
    #[test]
    fn a_default_start_creates_no_report_file_even_after_stop() {
        let root = temp_root("off");
        let mut s = mitm_settings_with_ports();
        s.observe = xt_core::model::ObserveSettings::default();
        let mut rt = MitmRuntime::default();
        rt.set_report_path_in(&root);
        rt.start(&s, vec![], None).expect("起代理");
        rt.stop();

        let entries: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(entries.is_empty(), "默认关不得产生任何落盘文件：{entries:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **判据②（落盘面）**：开启 + 命中 → 停 → 数据目录出现留档，
    /// 带结束时间，且**不含正文**。
    #[test]
    fn an_observed_run_persists_a_summary_on_stop_and_never_the_body() {
        let root = temp_root("on");
        let mut s = mitm_settings_with_ports();
        s.observe.enabled = true;
        s.observe.hosts = vec!["news.example".into()];
        let mut rt = MitmRuntime::default();
        rt.set_report_path_in(&root);
        rt.start(&s, vec![], None).expect("起代理");

        let ob = rt.observer.clone().expect("观察者");
        let body = br#"{"title":"SUPER_SECRET_BODY_TOKEN_9f2","promoted":true}"#;
        ob.observe(&observed_record("news.example", body), body);
        rt.stop();

        let path = crate::observe::report_path_in(&root);
        let text = std::fs::read_to_string(&path).expect("停之后留档必须存在");
        assert!(
            !text.contains("SUPER_SECRET_BODY_TOKEN_9f2"),
            "**留档里不许出现正文片段**：{text}"
        );
        assert!(text.contains("news.example"), "{text}");
        assert!(text.contains("\"ended_unix\": "), "停之后必须带结束时间：{text}");
        assert!(text.contains("\"exchanges\": 1"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 导出：相对路径被拒（可读原因）/ 没摘要被拒；有摘要时写到绝对路径，
    /// 且与数据目录留档的**摘要部分同源**。
    #[test]
    fn export_refuses_relative_paths_and_no_data_then_writes_the_same_document() {
        let root = temp_root("export");
        let mut s = mitm_settings_with_ports();
        s.observe.enabled = true;
        s.observe.hosts = vec!["news.example".into()];
        let mut rt = MitmRuntime::default();
        rt.set_report_path_in(&root);

        // 还没启动过 / 还没采到：给可读原因，不是静默失败。
        let err = rt.export_observe("/tmp/xraytun-never.json").unwrap_err();
        assert!(err.contains("还没有采到任何摘要"), "{err}");

        rt.start(&s, vec![], None).expect("起代理");
        let err = rt.export_observe("relative/leak.json").unwrap_err();
        assert!(err.contains("绝对路径"), "相对路径必须被拒：{err}");
        let err = rt.export_observe("/tmp/xraytun-never.json").unwrap_err();
        assert!(err.contains("还没有采到任何摘要"), "{err}");

        let ob = rt.observer.clone().expect("观察者");
        let body = br#"{"promoted":true}"#;
        ob.observe(&observed_record("news.example", body), body);

        let target = root.join("export.json");
        let written = rt.export_observe(target.to_str().unwrap()).expect("导出");
        assert_eq!(written, target.display().to_string());
        let exported = crate::observe::read_report_file(&target)
            .expect("读导出")
            .expect("导出文件必须存在");

        rt.stop();
        let persisted = crate::observe::read_report_file(&crate::observe::report_path_in(&root))
            .expect("读留档")
            .expect("停之后留档必须存在");
        assert_eq!(exported.exchanges, persisted.exchanges, "导出与留档同源");
        assert_eq!(exported.hosts, persisted.hosts, "导出与留档的摘要同源");
        assert_eq!(exported.configured_hosts, persisted.configured_hosts);
        assert_eq!(exported.markers, persisted.markers);
        assert_eq!(exported.schema, persisted.schema);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 清空：内存结论立刻归零（跑着的观察者看得到）；数据目录留档由命令层删除。
    #[test]
    fn clearing_observe_empties_the_live_ledger_in_place() {
        let root = temp_root("clear");
        let mut s = mitm_settings_with_ports();
        s.observe.enabled = true;
        s.observe.hosts = vec!["news.example".into()];
        let mut rt = MitmRuntime::default();
        rt.set_report_path_in(&root);
        rt.start(&s, vec![], None).expect("起代理");
        let ob = rt.observer.clone().expect("观察者");
        let body = br#"{"promoted":true}"#;
        ob.observe(&observed_record("news.example", body), body);
        assert_eq!(rt.status(&s, true).observe.exchanges, 1);

        rt.clear_observe();
        assert_eq!(rt.status(&s, true).observe.exchanges, 0, "清空必须立刻生效");
        // 跑着的观察者与运行态是**同一个 Arc**：新数据还是会继续采。
        ob.observe(&observed_record("news.example", body), body);
        assert_eq!(rt.status(&s, true).observe.exchanges, 1);
        rt.stop();
        let _ = std::fs::remove_dir_all(&root);
    }
}
