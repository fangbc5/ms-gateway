use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;
use dashmap::DashMap;
use serde::Deserialize;
use crate::config::SharedRouteRules;

/// 健康检查配置（从 routes.toml 的 [health_check] 段反序列化）
#[derive(Debug, Deserialize, Clone)]
pub struct HealthCheckConfig {
    /// 是否启用健康检查
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// 检查间隔（秒）
    #[serde(default = "default_interval")]
    pub interval_secs: u64,
    /// 单次检查超时（秒）
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    /// 健康检查路径
    #[serde(default = "default_path")]
    pub path: String,
    /// 连续失败 N 次标记为不健康
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,
    /// 连续成功 N 次恢复为健康
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,
}

fn default_enabled() -> bool { true }
fn default_interval() -> u64 { 10 }
fn default_timeout() -> u64 { 3 }
fn default_path() -> String { "/health".to_string() }
fn default_unhealthy_threshold() -> u32 { 3 }
fn default_healthy_threshold() -> u32 { 2 }

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            interval_secs: default_interval(),
            timeout_secs: default_timeout(),
            path: default_path(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
        }
    }
}

/// 单个上游的健康状态
#[derive(Debug)]
pub struct UpstreamHealth {
    pub url: String,
    pub healthy: AtomicBool,
    pub consecutive_failures: AtomicU32,
    pub consecutive_successes: AtomicU32,
}

impl UpstreamHealth {
    pub fn new(url: String) -> Self {
        Self {
            url,
            healthy: AtomicBool::new(true),
            consecutive_failures: AtomicU32::new(0),
            consecutive_successes: AtomicU32::new(0),
        }
    }

    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// 记录一次探测成功
    pub fn record_success(&self, healthy_threshold: u32) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        let successes = self.consecutive_successes.fetch_add(1, Ordering::Relaxed) + 1;

        if !self.is_healthy() && successes >= healthy_threshold {
            self.healthy.store(true, Ordering::Relaxed);
            tracing::info!("✅ 上游恢复健康: {}", self.url);
        }
    }

    /// 记录一次探测失败
    pub fn record_failure(&self, unhealthy_threshold: u32) {
        self.consecutive_successes.store(0, Ordering::Relaxed);
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;

        if self.is_healthy() && failures >= unhealthy_threshold {
            self.healthy.store(false, Ordering::Relaxed);
            tracing::warn!("❌ 上游标记为不健康: {}", self.url);
        }
    }
}

/// 共享健康状态表：key = upstream URL
pub type SharedHealthStatus = Arc<DashMap<String, Arc<UpstreamHealth>>>;

/// 创建共享健康状态表
pub fn create_health_status() -> SharedHealthStatus {
    Arc::new(DashMap::new())
}

/// 过滤出健康的上游节点。若全部不健康，返回所有节点（降级保护）
pub fn filter_healthy_upstreams(
    upstreams: &[String],
    health_status: &SharedHealthStatus,
) -> Vec<String> {
    let healthy: Vec<String> = upstreams
        .iter()
        .filter(|u| {
            health_status
                .get(*u)
                .map(|h| h.is_healthy())
                .unwrap_or(true) // 未记录的节点视为健康
        })
        .cloned()
        .collect();

    // 降级保护：全部不健康时返回所有节点
    if healthy.is_empty() {
        tracing::warn!("⚠️ 所有上游均不健康，降级使用全部节点");
        upstreams.to_vec()
    } else {
        healthy
    }
}

/// 启动后台健康检查任务
pub fn start_health_checker(
    config: HealthCheckConfig,
    shared_rules: SharedRouteRules,
    health_status: SharedHealthStatus,
) {
    if !config.enabled {
        tracing::info!("健康检查已禁用");
        return;
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs))
        .connect_timeout(Duration::from_secs(config.timeout_secs))
        .no_proxy()
        .build()
        .expect("Failed to build health check HTTP client");

    tracing::info!(
        "🏥 健康检查已启动: 间隔={}s, 超时={}s, 路径={}, 失败阈值={}, 恢复阈值={}",
        config.interval_secs, config.timeout_secs, config.path,
        config.unhealthy_threshold, config.healthy_threshold,
    );

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(config.interval_secs));

        loop {
            interval.tick().await;

            // 从最新路由规则中收集所有上游 URL
            let rules = shared_rules.load();
            let all_upstreams: Vec<String> = rules
                .iter()
                .flat_map(|r| r.upstream.iter().cloned())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();

            // 确保新上游被纳入健康状态表
            for url in &all_upstreams {
                health_status
                    .entry(url.clone())
                    .or_insert_with(|| Arc::new(UpstreamHealth::new(url.clone())));
            }

            // 清理不再存在的上游
            health_status.retain(|url, _| all_upstreams.contains(url));

            // 并发探测所有上游
            let mut handles = Vec::new();
            for url in &all_upstreams {
                let client = client.clone();
                let check_url = format!("{}{}", url, config.path);
                let health = health_status.get(url).map(|h| h.clone());
                let unhealthy_threshold = config.unhealthy_threshold;
                let healthy_threshold = config.healthy_threshold;

                handles.push(tokio::spawn(async move {
                    if let Some(health) = health {
                        match client.get(&check_url).send().await {
                            Ok(resp) if resp.status().is_success() => {
                                health.record_success(healthy_threshold);
                                tracing::debug!("健康检查通过: {}", check_url);
                            }
                            Ok(resp) => {
                                health.record_failure(unhealthy_threshold);
                                tracing::debug!(
                                    "健康检查失败: {} (HTTP {})",
                                    check_url, resp.status()
                                );
                            }
                            Err(e) => {
                                health.record_failure(unhealthy_threshold);
                                tracing::debug!("健康检查失败: {} ({})", check_url, e);
                            }
                        }
                    }
                }));
            }

            // 等待所有探测完成
            for handle in handles {
                let _ = handle.await;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_upstream_health_default_healthy() {
        let health = UpstreamHealth::new("http://localhost:3000".to_string());
        assert!(health.is_healthy());
        assert_eq!(health.consecutive_failures.load(Ordering::Relaxed), 0);
        assert_eq!(health.consecutive_successes.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_unhealthy_after_threshold() {
        let health = UpstreamHealth::new("http://localhost:3000".to_string());

        // 连续失败 2 次（阈值=3），仍然健康
        health.record_failure(3);
        health.record_failure(3);
        assert!(health.is_healthy());

        // 第 3 次失败，标记为不健康
        health.record_failure(3);
        assert!(!health.is_healthy());
    }

    #[test]
    fn test_recovery_after_threshold() {
        let health = UpstreamHealth::new("http://localhost:3000".to_string());

        // 先变为不健康
        for _ in 0..3 {
            health.record_failure(3);
        }
        assert!(!health.is_healthy());

        // 连续成功 1 次（阈值=2），仍然不健康
        health.record_success(2);
        assert!(!health.is_healthy());

        // 第 2 次成功，恢复健康
        health.record_success(2);
        assert!(health.is_healthy());
    }

    #[test]
    fn test_failure_resets_success_counter() {
        let health = UpstreamHealth::new("http://localhost:3000".to_string());

        // 先变为不健康
        for _ in 0..3 {
            health.record_failure(3);
        }
        assert!(!health.is_healthy());

        // 成功 1 次，然后失败 → 成功计数器被重置
        health.record_success(2);
        health.record_failure(3);
        assert_eq!(health.consecutive_successes.load(Ordering::Relaxed), 0);

        // 需要重新连续成功 2 次才能恢复
        health.record_success(2);
        assert!(!health.is_healthy());
        health.record_success(2);
        assert!(health.is_healthy());
    }

    #[test]
    fn test_success_resets_failure_counter() {
        let health = UpstreamHealth::new("http://localhost:3000".to_string());

        // 失败 2 次（未达阈值 3）
        health.record_failure(3);
        health.record_failure(3);

        // 成功 1 次 → 失败计数器被重置
        health.record_success(2);
        assert_eq!(health.consecutive_failures.load(Ordering::Relaxed), 0);

        // 再失败 2 次仍然健康（因为计数器被重置了）
        health.record_failure(3);
        health.record_failure(3);
        assert!(health.is_healthy());
    }

    #[test]
    fn test_filter_healthy_upstreams() {
        let status = create_health_status();
        let upstreams = vec![
            "http://a:3000".to_string(),
            "http://b:3000".to_string(),
            "http://c:3000".to_string(),
        ];

        // 初始：所有未注册 → 视为健康
        let result = filter_healthy_upstreams(&upstreams, &status);
        assert_eq!(result.len(), 3);

        // 注册并标记 b 为不健康
        let health_b = Arc::new(UpstreamHealth::new("http://b:3000".to_string()));
        for _ in 0..3 {
            health_b.record_failure(3);
        }
        status.insert("http://b:3000".to_string(), health_b);

        let result = filter_healthy_upstreams(&upstreams, &status);
        assert_eq!(result.len(), 2);
        assert!(!result.contains(&"http://b:3000".to_string()));
    }

    #[test]
    fn test_filter_all_unhealthy_fallback() {
        let status = create_health_status();
        let upstreams = vec![
            "http://a:3000".to_string(),
            "http://b:3000".to_string(),
        ];

        // 标记全部不健康
        for url in &upstreams {
            let health = Arc::new(UpstreamHealth::new(url.clone()));
            for _ in 0..3 {
                health.record_failure(3);
            }
            status.insert(url.clone(), health);
        }

        // 降级保护：全部不健康时返回所有节点
        let result = filter_healthy_upstreams(&upstreams, &status);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_health_check_config_default() {
        let config = HealthCheckConfig::default();
        assert!(config.enabled);
        assert_eq!(config.interval_secs, 10);
        assert_eq!(config.timeout_secs, 3);
        assert_eq!(config.path, "/health");
        assert_eq!(config.unhealthy_threshold, 3);
        assert_eq!(config.healthy_threshold, 2);
    }
}
