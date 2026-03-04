pub mod round_robin;
pub mod weighted_random;
pub mod ip_hash;

use std::net::SocketAddr;

pub trait LoadBalancer: Send + Sync {
    fn select(&self, client_ip: Option<&SocketAddr>) -> Option<String>;
}

pub use round_robin::RoundRobinBalancer;
pub use weighted_random::WeightedRandomBalancer;
pub use weighted_random::WeightedUpstream;
pub use ip_hash::IpHashBalancer;