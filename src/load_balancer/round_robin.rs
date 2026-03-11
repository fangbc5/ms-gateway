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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_robin_cycling() {
        let balancer = RoundRobinBalancer::new(vec![
            "http://a:3000".into(),
            "http://b:3000".into(),
            "http://c:3000".into(),
        ]);

        assert_eq!(balancer.select(None).unwrap(), "http://a:3000");
        assert_eq!(balancer.select(None).unwrap(), "http://b:3000");
        assert_eq!(balancer.select(None).unwrap(), "http://c:3000");
        // 循环回到第一个
        assert_eq!(balancer.select(None).unwrap(), "http://a:3000");
    }

    #[test]
    fn test_empty_upstreams() {
        let balancer = RoundRobinBalancer::new(vec![]);
        assert!(balancer.select(None).is_none());
    }

    #[test]
    fn test_dynamic_update() {
        let balancer = RoundRobinBalancer::new(vec!["http://a:3000".into()]);
        assert_eq!(balancer.select(None).unwrap(), "http://a:3000");

        // 动态更新上游
        balancer.update_upstreams(vec!["http://x:3000".into(), "http://y:3000".into()]);
        let result = balancer.select(None).unwrap();
        assert!(result == "http://x:3000" || result == "http://y:3000");
    }
}
