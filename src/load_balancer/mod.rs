pub mod round_robin;
pub mod weighted_random;
pub mod ip_hash;

use std::net::SocketAddr;

/// 单个上游节点及权重
#[derive(Debug, Clone)]
pub struct WeightedUpstream {
    pub url: String,
    pub weight: u32,
}

pub trait LoadBalancer: Send + Sync {
    fn select(&self, client_ip: Option<&SocketAddr>) -> Option<String>;
    /// 原地更新上游节点列表（无锁，线程安全）
    /// 接收带权重的上游列表，各实现按自身逻辑处理权重
    fn update_upstreams(&self, new_upstreams: Vec<WeightedUpstream>);
}

pub use round_robin::RoundRobinBalancer;
pub use weighted_random::WeightedRandomBalancer;
pub use ip_hash::IpHashBalancer;