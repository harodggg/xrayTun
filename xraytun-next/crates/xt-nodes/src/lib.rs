//! xt-nodes —— 节点目录：订阅 → 节点视图 + 选择（不做回落）
//!
//! 所有者：backend-2。职责边界见 docs/architecture/00-CONTRACT-FREEZE.md。
//!
//! 这个 crate 是 xt-subs（解析结果）与 xt-xrayconf（配置生成）之间的转换层：
//! xt-xrayconf 只认识 `OutboundSpec`，不认识 `ParsedNode`，转换只有这一处。
//!
//! ## 选择（selection）的三条硬规则
//!
//! 1. **只记录，不预判**：`select()` 不测延迟、不看历史、不因为节点看起来不可用
//!    就拒绝 —— 可用性是连接时的事实，不是目录能猜的。
//! 2. **不自动换下一个**：切过去失败就停在失败（I2）。
//! 3. **目录里没有就是 `NotFound`**：订阅刷新后旧 id 可能消失，
//!    这时必须如实报错，而不是悄悄选列表里的第一个 —— 那会让用户以为
//!    「还是原来那台服务器」。
//!
//! `restore_selection` 与 `select` 的区别正在于此：从设置里恢复记忆时目录可能
//! 还空着（daemon 启动顺序），所以恢复**不校验**存在性；校验发生在真正要用它的
//! 那一刻（`selected_view` / `outbound_spec`）。

use std::path::Path;

use xt_contract::error::{not_found, ErrorBody};
use xt_contract::model::{NodeId, NodeSource, NodeView};
use xt_subs::ParsedNode;
use xt_xrayconf::OutboundSpec;

/// 一个来源（订阅 / 手工）对应的一组节点。
#[derive(Clone, Debug)]
struct SourceNodes {
    source: NodeSource,
    nodes: Vec<ParsedNode>,
}

/// 节点目录。内部是「按来源分组 + 当前选择」，没有索引/缓存这类会过期的派生状态。
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    sources: Vec<SourceNodes>,
    selected: Option<NodeId>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// 整份替换某个来源的节点（订阅刷新、手工导入都用这一条路径）。
    ///
    /// 「替换」而不是「合并」：订阅刷新后消失的节点必须从目录里消失，
    /// 否则用户会看到一个已经下线的节点并且还能选中它。
    pub fn replace_source(&mut self, source: NodeSource, nodes: Vec<ParsedNode>) {
        match self.sources.iter_mut().find(|entry| entry.source == source) {
            Some(entry) => entry.nodes = nodes,
            None => self.sources.push(SourceNodes { source, nodes }),
        }
    }

    /// 移除一个来源（订阅被删除时用）。
    pub fn remove_source(&mut self, source: &NodeSource) {
        self.sources.retain(|entry| entry.source != *source);
    }

    /// 目录里全部节点，按来源插入顺序、来源内保持解析顺序。
    ///
    /// 同一个 NodeId 只出现一次（不同订阅收录同一台服务器很常见）：
    /// 保留第一次出现的那个，来源归属因此是确定的。
    /// `probe` 恒为 `None` —— 目录不产生延迟数据（I3）；探测结果由 xt-probe
    /// 采样后由调用方合并。
    pub fn list(&self) -> Vec<NodeView> {
        let mut seen = std::collections::HashSet::new();
        let mut views = Vec::new();
        for entry in &self.sources {
            for node in &entry.nodes {
                if !seen.insert(node.id.clone()) {
                    continue;
                }
                views.push(NodeView {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    protocol: node.protocol.clone(),
                    endpoint: node.endpoint.clone(),
                    source: entry.source.clone(),
                    probe: None,
                });
            }
        }
        views
    }

    pub fn get(&self, id: &NodeId) -> Option<&ParsedNode> {
        self.sources.iter().flat_map(|entry| entry.nodes.iter()).find(|node| node.id == *id)
    }

    /// 选择节点。只记录，不预判可用性；目录里没有 → `NotFound`。
    pub fn select(&mut self, id: &NodeId) -> Result<(), ErrorBody> {
        if self.get(id).is_none() {
            return Err(not_found(format!("节点 {id} 不在当前目录里"))
                .with_detail(serde_json::json!({ "node_id": id.as_str() })));
        }
        self.selected = Some(id.clone());
        Ok(())
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selected.clone()
    }

    /// 从设置里恢复记忆。**不校验存在性**：目录可能还没装载，
    /// 而在恢复这一步「找不到就换一个」正是我们要禁止的回落。
    pub fn restore_selection(&mut self, id: Option<NodeId>) {
        self.selected = id;
    }

    /// 当前选择对应的视图。
    ///
    /// * 没有选择 → `Ok(None)`（如实表达「还没选」）；
    /// * 选了但目录里没有（订阅刷新后旧 id 消失）→ `Err(NotFound)`。
    pub fn selected_view(&self) -> Result<Option<NodeView>, ErrorBody> {
        let Some(id) = &self.selected else {
            return Ok(None);
        };
        let view = self
            .list()
            .into_iter()
            .find(|view| view.id == *id)
            .ok_or_else(|| {
                not_found(format!("已选节点 {id} 已不在当前目录里（订阅可能已刷新），不自动改选"))
                    .with_detail(serde_json::json!({ "selected_node": id.as_str() }))
            })?;
        Ok(Some(view))
    }

    /// 转成 xt-xrayconf 认识的最小形状。缺失同样是 `NotFound`。
    pub fn outbound_spec(&self, id: &NodeId) -> Result<OutboundSpec, ErrorBody> {
        let node = self.get(id).ok_or_else(|| {
            not_found(format!("节点 {id} 不在当前目录里，无法生成配置"))
                .with_detail(serde_json::json!({ "node_id": id.as_str() }))
        })?;
        Ok(OutboundSpec { node_id: node.id.clone(), outbound: node.outbound.clone() })
    }

    /// 读取设置文件里记住的选择。文件损坏 → 错误（由 xt-settings 保证），
    /// 不会退化成「没有选择」。
    pub fn load_selection(path: &Path) -> Result<Option<NodeId>, ErrorBody> {
        Ok(xt_settings::load(path)?.selected_node)
    }

    /// 把当前选择写回设置文件（原子替换）。选择是用户状态的一部分，
    /// 与其它设置字段共存于同一个文件，不另开一个存储。
    pub fn persist_selection(&self, path: &Path) -> Result<(), ErrorBody> {
        let mut settings = xt_settings::load(path)?;
        settings.selected_node = self.selected.clone();
        xt_settings::save(path, &settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use xt_contract::error::ErrorCode;
    use xt_contract::model::SubscriptionId;

    fn parsed(protocol: &str, host: &str, name: &str) -> ParsedNode {
        // 用真实解析器构造测试数据，避免测试夹具与生产形状漂移。
        let line = format!("{protocol}://uuid-{name}@{host}:443?encryption=none&security=none#{name}");
        xt_subs::parse_links(&line).expect("构造测试节点").remove(0)
    }

    fn sub_source(id: &str) -> NodeSource {
        NodeSource::Subscription { id: SubscriptionId::new(id) }
    }

    #[test]
    fn replace_and_list_keeps_source_attribution() {
        let mut catalog = Catalog::new();
        catalog.replace_source(sub_source("s1"), vec![parsed("vless", "a.example.com", "A")]);
        catalog.replace_source(NodeSource::Manual, vec![parsed("trojan", "b.example.com", "B")]);

        let views = catalog.list();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].source, sub_source("s1"));
        assert_eq!(views[0].probe, None, "目录不产生延迟数据");
        assert_eq!(views[1].source, NodeSource::Manual);
    }

    #[test]
    fn duplicate_node_across_sources_appears_once() {
        let mut catalog = Catalog::new();
        let node = parsed("vless", "a.example.com", "A");
        catalog.replace_source(sub_source("s1"), vec![node.clone()]);
        catalog.replace_source(NodeSource::Manual, vec![node]);
        assert_eq!(catalog.list().len(), 1);
    }

    #[test]
    fn select_unknown_node_is_not_found() {
        let mut catalog = Catalog::new();
        catalog.replace_source(sub_source("s1"), vec![parsed("vless", "a.example.com", "A")]);
        let err = catalog.select(&NodeId::new("does-not-exist")).expect_err("必须 not_found");
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    }

    /// 核心断言：订阅刷新后旧节点 id 变为 not_found，**不自动改选第一个**。
    #[test]
    fn refresh_removing_the_selected_node_reports_not_found() {
        let mut catalog = Catalog::new();
        let old = parsed("vless", "old.example.com", "OLD");
        catalog.replace_source(sub_source("s1"), vec![old.clone()]);
        catalog.select(&old.id).expect("选择");
        assert!(catalog.selected_view().expect("还在").is_some());

        // 刷新后旧节点消失，新节点换了主机名。
        catalog.replace_source(sub_source("s1"), vec![parsed("vless", "new.example.com", "NEW")]);

        assert_eq!(catalog.selected(), Some(old.id.clone()), "记忆本身不被改写");
        let err = catalog.selected_view().expect_err("必须 not_found");
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
        assert!(catalog.outbound_spec(&old.id).is_err(), "旧 id 不能生成配置");
    }

    #[test]
    fn restore_selection_does_not_validate_or_replace() {
        let mut catalog = Catalog::default();
        // 目录还是空的（daemon 启动顺序如此），恢复记忆不应报错。
        catalog.restore_selection(Some(NodeId::new("from-settings")));
        assert_eq!(catalog.selected(), Some(NodeId::new("from-settings")));
        // 但真正要用的时候立刻如实报错。
        let err = catalog.selected_view().expect_err("必须 not_found");
        assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    }

    #[test]
    fn outbound_spec_carries_node_id_and_content_without_tag() {
        let mut catalog = Catalog::new();
        let node = parsed("vless", "a.example.com", "A");
        catalog.replace_source(sub_source("s1"), vec![node.clone()]);
        let spec = catalog.outbound_spec(&node.id).expect("转成 OutboundSpec");
        assert_eq!(spec.node_id, node.id);
        assert_eq!(spec.outbound["protocol"], json!("vless"));
        assert!(spec.outbound.get("tag").is_none(), "tag 由 xt-xrayconf 写入");
    }

    #[test]
    fn selection_round_trips_through_settings_file() {
        let dir = std::env::temp_dir().join(format!("xt-nodes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建目录");
        let path = dir.join("settings.json");

        let mut catalog = Catalog::new();
        let node = parsed("vless", "a.example.com", "A");
        catalog.replace_source(sub_source("s1"), vec![node.clone()]);
        catalog.select(&node.id).expect("选择");
        catalog.persist_selection(&path).expect("落盘");

        assert_eq!(Catalog::load_selection(&path).expect("读回"), Some(node.id));
    }
}
