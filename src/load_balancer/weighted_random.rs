use std::net::SocketAddr;
use std::sync::Arc;
use rand::Rng;
use arc_swap::ArcSwap;
use crate::load_balancer::{LoadBalancer, WeightedUpstream};

/// 内部不可变负载均衡器，支持高效随机选择
#[derive(Debug)]
struct WeightedRandomBalancerInner {
    /// 仅包含 weight > 0 的上游节点（已过滤）
    upstreams: Vec<WeightedUpstream>,
    prefix_sums: Vec<u32>, // 前缀和，与 upstreams 索引一一对应
    total_weight: u32,
}

impl WeightedRandomBalancerInner {
    pub fn new(upstreams: Vec<WeightedUpstream>) -> Self {
        // 过滤掉 weight=0 的节点，确保 upstreams 与 prefix_sums 索引对齐
        let active_upstreams: Vec<WeightedUpstream> = upstreams
            .into_iter()
            .filter(|u| u.weight > 0)
            .collect();

        let mut prefix_sums = Vec::with_capacity(active_upstreams.len());
        let mut total_weight = 0;

        for u in &active_upstreams {
            total_weight += u.weight;
            prefix_sums.push(total_weight);
        }

        Self {
            upstreams: active_upstreams,
            prefix_sums,
            total_weight,
        }
    }

    /// O(log n) 随机选择
    pub fn select(&self) -> Option<String> {
        if self.upstreams.is_empty() || self.total_weight == 0 {
            return None;
        }

        let mut rng = rand::thread_rng();
        let random_weight = rng.gen_range(1..=self.total_weight);

        match self.prefix_sums.binary_search(&random_weight) {
            Ok(idx) => Some(self.upstreams[idx].url.clone()),
            Err(idx) => Some(self.upstreams[idx].url.clone()),
        }
    }
}

/// 高性能线程安全带权随机负载均衡器
#[derive(Debug)]
pub struct WeightedRandomBalancer {
    inner: ArcSwap<WeightedRandomBalancerInner>,
}

impl WeightedRandomBalancer {
    /// 创建新负载均衡器
    pub fn new(upstreams: Vec<WeightedUpstream>) -> Self {
        Self {
            inner: ArcSwap::from_pointee(WeightedRandomBalancerInner::new(upstreams)),
        }
    }

    /// 高并发安全随机选择
    pub fn select_inner(&self) -> Option<String> {
        self.inner.load().select()
    }
}

impl LoadBalancer for WeightedRandomBalancer {
    fn select(&self, _client_ip: Option<&SocketAddr>) -> Option<String> {
        self.select_inner()
    }

    fn update_upstreams(&self, new_upstreams: Vec<WeightedUpstream>) {
        let new_inner = WeightedRandomBalancerInner::new(new_upstreams);
        self.inner.store(Arc::new(new_inner));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_selection() {
        let balancer = WeightedRandomBalancer::new(vec![
            WeightedUpstream { url: "http://localhost:30000".to_string(), weight: 1 },
            WeightedUpstream { url: "http://localhost:30001".to_string(), weight: 2 },
            WeightedUpstream { url: "http://localhost:30002".to_string(), weight: 3 },
        ]);

        let mut counts = std::collections::HashMap::new();
        for _ in 0..6000 {
            let url = balancer.select(None).unwrap();
            *counts.entry(url).or_insert(0) += 1;
        }

        // 检查大致比例
        println!("{:?}", counts);
        assert!(counts["http://localhost:30000"] < counts["http://localhost:30001"]);
        assert!(counts["http://localhost:30001"] < counts["http://localhost:30002"]);
    }

    #[test]
    fn test_dynamic_update() {
        let balancer = WeightedRandomBalancer::new(vec![
            WeightedUpstream { url: "http://localhost:30000".to_string(), weight: 1 },
        ]);
        assert_eq!(balancer.select(None).unwrap(), "http://localhost:30000");

        // 原地更新上游节点
        balancer.update_upstreams(vec![
            WeightedUpstream { url: "http://localhost:30001".to_string(), weight: 1 },
            WeightedUpstream { url: "http://localhost:30002".to_string(), weight: 1 },
        ]);

        // 更新后应选到新节点
        let selected = balancer.select(None).unwrap();
        assert!(selected == "http://localhost:30001" || selected == "http://localhost:30002");
    }

    #[test]
    fn test_update_to_empty() {
        let balancer = WeightedRandomBalancer::new(vec![
            WeightedUpstream { url: "http://localhost:30000".to_string(), weight: 1 },
        ]);
        assert!(balancer.select(None).is_some());

        balancer.update_upstreams(vec![] as Vec<WeightedUpstream>);
        assert!(balancer.select(None).is_none());
    }
}

