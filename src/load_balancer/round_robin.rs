use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use arc_swap::ArcSwap;
use std::net::SocketAddr;
use crate::load_balancer::{LoadBalancer, WeightedUpstream};

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

    fn update_upstreams(&self, new_upstreams: Vec<WeightedUpstream>) {
        let urls: Vec<String> = new_upstreams.into_iter().map(|u| u.url).collect();
        self.upstreams.store(Arc::new(urls));
        // 不重置计数器，让 fetch_add % new_len 自然分布
        // 避免 Nacos 频繁推送导致流量始终倾斜到第一个节点
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
        let balancer = RoundRobinBalancer::new(vec![
            "http://a:3000".into(),
            "http://b:3000".into(),
        ]);
        assert_eq!(balancer.select(None).unwrap(), "http://a:3000");

        // 原地更新上游节点
        balancer.update_upstreams(vec![
            WeightedUpstream { url: "http://c:3000".into(), weight: 1 },
            WeightedUpstream { url: "http://d:3000".into(), weight: 1 },
        ]);

        // 更新后计数器不重置，fetch_add % new_len 自然分布
        let next = balancer.select(None).unwrap();
        assert!(next == "http://c:3000" || next == "http://d:3000");
    }

    #[test]
    fn test_update_to_empty() {
        let balancer = RoundRobinBalancer::new(vec!["http://a:3000".into()]);
        assert!(balancer.select(None).is_some());

        balancer.update_upstreams(vec![] as Vec<WeightedUpstream>);
        assert!(balancer.select(None).is_none());
    }
}

