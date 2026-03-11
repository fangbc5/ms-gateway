use nacos_rust_client::client::naming_client::{Instance, InstanceListener, ServiceInstanceKey, QueryInstanceListParams};
use std::sync::Arc;

use crate::config::{NacosSettings, Settings};

/// 订阅服务发现（监听路由中配置的 service_name 对应的实例变更）
pub async fn subscribe_services(config: &NacosSettings) {
    if config.subscribe_services.is_empty() {
        return;
    }

    let naming_client = match super::client::get_naming_client() {
        Some(c) => c,
        None => return,
    };

    let group = config.group.clone();

    for service_name in &config.subscribe_services {
        let svc = service_name.clone();
        let grp = group.clone();

        struct SvcListener { service_name: String, group: String }

        impl InstanceListener for SvcListener {
            fn get_key(&self) -> ServiceInstanceKey {
                ServiceInstanceKey::new(&self.service_name, &self.group)
            }

            fn change(
                &self,
                _key: &ServiceInstanceKey,
                value: &Vec<Arc<Instance>>,
                add_list: &Vec<Arc<Instance>>,
                remove_list: &Vec<Arc<Instance>>,
            ) {
                tracing::info!(
                    "🔄 服务 {} 实例变更 (全量:{}, 新增:{}, 移除:{})",
                    self.service_name, value.len(), add_list.len(), remove_list.len()
                );
                super::update_service_instances(&self.service_name, value.clone());

                for inst in value {
                    tracing::debug!(
                        "  - {}:{} (healthy: {})",
                        inst.ip, inst.port, inst.healthy
                    );
                }
            }
        }

        let listener = Box::new(SvcListener {
            service_name: svc.clone(),
            group: grp.clone(),
        });

        match naming_client.subscribe(listener).await {
            Ok(_) => tracing::info!("📡 已订阅服务发现: {}", svc),
            Err(e) => {
                tracing::error!("订阅服务 {} 失败: {}", svc, e);
                continue;
            }
        }

        // 立即获取当前实例列表
        let params = QueryInstanceListParams::new_simple(&svc, &grp);
        match naming_client.query_instances(params).await {
            Ok(instances) => {
                tracing::info!("获取 {} 当前实例: {} 个", svc, instances.len());
                super::update_service_instances(&svc, instances);
            }
            Err(e) => tracing::warn!("获取 {} 当前实例失败: {}", svc, e),
        }
    }
}

/// 将网关自身注册到 Nacos
pub async fn register_self(nacos_config: &NacosSettings, settings: &Settings) {
    let naming_client = match super::client::get_naming_client() {
        Some(c) => c,
        None => {
            tracing::error!("无法注册网关：NamingClient 未初始化");
            return;
        }
    };

    let service_name = nacos_config.service_name.clone()
        .unwrap_or_else(|| "ms-gateway".to_string());

    // 解析绑定地址
    let (ip, port) = parse_bind_addr(&settings.gateway_bind);

    let instance = Instance::new_simple(
        &ip, port, &service_name, &nacos_config.group,
    );

    naming_client.register(instance);
    tracing::info!("✅ 网关已注册到 Nacos: {}:{} (服务名: {})", ip, port, service_name);
}

/// 解析绑定地址为 (ip, port)
fn parse_bind_addr(bind: &str) -> (String, u32) {
    if let Some(idx) = bind.rfind(':') {
        let ip = bind[..idx].to_string();
        let port = bind[idx + 1..].parse().unwrap_or(8080);
        // 如果是 0.0.0.0 则尝试获取本机 IP
        let ip = if ip == "0.0.0.0" {
            local_ip().unwrap_or_else(|| "127.0.0.1".to_string())
        } else {
            ip
        };
        (ip, port)
    } else {
        ("127.0.0.1".to_string(), 8080)
    }
}

/// 获取本机 IP
fn local_ip() -> Option<String> {
    use std::net::UdpSocket;
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    socket.local_addr().ok().map(|a| a.ip().to_string())
}
