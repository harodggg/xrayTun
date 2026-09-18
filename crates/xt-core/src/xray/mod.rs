//! Xray 集成层：配置生成、进程生命周期、延迟探针。

pub mod config;
pub mod probe;
pub mod process;
pub mod stats;

pub use config::{
    build, build_pretty, lint_node, merge_rules, node_to_outbound, tun_inbound_spec,
    CoreConfigInput, InboundProfile, TunInboundSpec, API_PORT, MIN_CORE_VERSION_NATIVE_TUN,
};
pub use probe::{probe_nodes, ProbeOptions, ProbeResult, DEFAULT_PROBE_URL};
pub use stats::{
    parse_traffic_counter, query_stats, traffic_by_tag, traffic_from_stats, CounterParts,
    StatEntry, TrafficCounters, QUERY_STATS_PATH,
};
pub use process::{
    core_version, resolve_core_binary, validate_config, wait_for_port, CoreEvent, LogStream,
    XrayProcess,
};
