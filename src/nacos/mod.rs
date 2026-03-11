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

/// 将 Nacos 服务实例转换为 upstream URL 列表
pub fn instances_to_upstreams(service_name: &str) -> Vec<String> {
    get_service_instances(service_name)
        .map(|instances| {
            instances
                .iter()
                .filter(|i| i.healthy)
                .map(|i| format!("http://{}:{}", i.ip, i.port))
                .collect()
        })
        .unwrap_or_default()
}


/// 条件初始化 Nacos（配置开关控制）
pub async fn init_if_enabled(settings: &Settings, shared_rules: &SharedRouteRules) {
    let nacos_settings = match &settings.nacos {
        Some(ns) if ns.enabled => ns,
        _ => {
            tracing::info!("Nacos 未启用，使用本地配置");
            return;
        }
    };

    // 1. 初始化客户端
    if let Err(e) = client::init_nacos(nacos_settings).await {
        tracing::error!("Nacos 客户端初始化失败: {}，回退到本地配置", e);
        return;
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
}
