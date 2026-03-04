use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use arc_swap::ArcSwap;
use std::net::SocketAddr;
use crate::load_balancer::LoadBalancer;

#[derive(Debug)]
pub struct RoundRobinBalancer {
    upstreams: ArcSwap<Vec<String>>,
    current: AtomicUsize,
}

impl RoundRobinBalancer {
    pub fn new(upstreams: Vec<String>) -> Self {
        Self {
            upstreams: ArcSwap::from_pointee(upstreams),
            current: AtomicUsize::new(0),
        }
    }

    /// 无锁更新节点列表
    #[allow(dead_code)]
    pub fn update_upstreams(&self, new_upstreams: Vec<String>) {
        self.upstreams.store(Arc::new(new_upstreams));
    }

    /// 获取当前节点列表
    #[allow(dead_code)]
    pub fn get_upstreams(&self) -> Arc<Vec<String>> {
        self.upstreams.load_full()
    }
}

impl LoadBalancer for RoundRobinBalancer {
    fn select(&self, _client_ip: Option<&SocketAddr>) -> Option<String> {
        let ups = self.upstreams.load();
        if ups.is_empty() {
            return None;
        }

        let index = self.current.fetch_add(1, Ordering::Relaxed) % ups.len();
        ups.get(index).cloned()
    }
}
