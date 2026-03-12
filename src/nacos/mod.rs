mod client;
mod nacos_config;
pub(crate) mod discovery;

use dashmap::DashMap;
use nacos_rust_client::client::naming_client::Instance;
use once_cell::sync::Lazy;
use std::sync::Arc;

use crate::config::{Settings, SharedRouteRules};

/// 服务实例存储：服务名 -> 实例列表
static SERVICE_INSTANCES: Lazy<DashMap<String, Vec<Arc<Instance>>>> = Lazy::new(DashMap::new);

/// 获取指定服务的实例列表
pub fn get_service_instances(service_name: &str) -> Option<Vec<Arc<Instance>>> {
    SERVICE_INSTANCES.get(service_name).map(|e| e.value().clone())
}

/// 更新服务实例列表
fn update_service_instances(service_name: &str, instances: Vec<Arc<Instance>>) {
    SERVICE_INSTANCES.insert(service_name.to_string(), instances);
}

/// 将 Nacos 服务实例转换为带权重的 upstream 列表（保留 Nacos 权重）
pub fn instances_to_weighted_upstreams(service_name: &str) -> Vec<crate::load_balancer::WeightedUpstream> {
    get_service_instances(service_name)
        .map(|instances| {
            instances
                .iter()
                .filter(|i| i.healthy)
                .map(|i| crate::load_balancer::WeightedUpstream {
                    url: format!("http://{}:{}", i.ip, i.port),
                    weight: i.weight.round().max(1.0) as u32, // Nacos weight f32 → u32，最小为 1
                })
                .collect()
        })
        .unwrap_or_default()
}


/// 条件初始化 Nacos（配置开关控制）
///
/// 返回 `true` 表示 Nacos 路由配置已成功加载（无需加载本地文件），
/// 返回 `false` 表示 Nacos 未启用或初始化失败（应 fallback 到文件配置）。
pub async fn init_if_enabled(settings: &Settings, shared_rules: &SharedRouteRules) -> bool {
    let nacos_settings = match &settings.nacos {
        Some(ns) if ns.enabled => ns,
        _ => {
            tracing::info!("Nacos 未启用，使用本地配置");
            return false;
        }
    };

    // 1. 初始化客户端
    if let Err(e) = client::init_nacos(nacos_settings).await {
        tracing::error!("Nacos 客户端初始化失败: {}，回退到本地配置", e);
        return false;
    }

    // 2. 从 Nacos 拉取路由配置并订阅变更（合并为一步，避免重复加载）
    //    订阅时 listener 会立即收到当前配置值，等同于 fetch + subscribe
    nacos_config::subscribe_route_changes(nacos_settings, shared_rules).await;

    // 3. 注册网关自身 + 订阅自身服务
    if nacos_settings.register_enabled {
        discovery::register_self(nacos_settings, settings).await;
        // 自注册后也订阅自身，方便其他路由引用
        let self_name = nacos_settings.service_name.clone()
            .unwrap_or_else(|| "ms-gateway".to_string());
        discovery::subscribe_one_service(&self_name, &nacos_settings.naming_group).await;
    }

    // 判断 Nacos 路由是否已经成功加载
    let active = crate::config::is_nacos_routes_active();
    if active {
        tracing::info!("✅ Nacos 路由配置已激活，跳过本地 routes.toml 加载和监听");
    } else {
        tracing::warn!("⚠ Nacos 已连接但未加载路由配置（routes_data_id 未配置或配置为空），回退到本地配置");
    }
    active
}
