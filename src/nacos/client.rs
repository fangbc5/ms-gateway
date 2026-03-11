use crate::config::NacosSettings;
use nacos_rust_client::client::AuthInfo;
use once_cell::sync::OnceCell;
use std::sync::Arc;

/// Nacos 命名客户端（服务注册/发现）
static NAMING_CLIENT: OnceCell<Arc<nacos_rust_client::client::naming_client::NamingClient>> =
    OnceCell::new();

/// Nacos 配置客户端（配置管理）
static CONFIG_CLIENT: OnceCell<Arc<nacos_rust_client::client::config_client::ConfigClient>> =
    OnceCell::new();

/// 初始化 Nacos 客户端
pub async fn init_nacos(config: &NacosSettings) -> Result<(), anyhow::Error> {
    use nacos_rust_client::client::ClientBuilder;

    let server_addr = &config.server_addrs;

    let auth_info: Option<AuthInfo> =
        match (&config.username, &config.password) {
            (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {
                Some(AuthInfo::new(u, p))
            }
            _ => None,
        };

    let mut builder = ClientBuilder::new()
        .set_endpoint_addrs(server_addr)
        .set_auth_info(auth_info.clone())
        .set_use_grpc(true);

    if let Some(ns) = &config.namespace {
        builder = builder.set_tenant(ns.to_string());
    }

    let app_name = config.service_name.clone().unwrap_or_else(|| "ms-gateway".to_string());
    builder = builder.set_app_name(app_name);

    let (config_client, naming_client) = builder.build();

    NAMING_CLIENT.set(naming_client)
        .map_err(|_| anyhow::anyhow!("Nacos NamingClient 已初始化"))?;
    CONFIG_CLIENT.set(config_client)
        .map_err(|_| anyhow::anyhow!("Nacos ConfigClient 已初始化"))?;

    tracing::info!(
        "✅ Nacos 客户端初始化成功 (服务器: {}, 命名空间: {:?}, 认证: {})",
        server_addr,
        config.namespace,
        if auth_info.is_some() { "已启用" } else { "未启用" }
    );

    Ok(())
}

/// 获取 NamingClient
pub fn get_naming_client() -> Option<Arc<nacos_rust_client::client::naming_client::NamingClient>> {
    NAMING_CLIENT.get().cloned()
}

/// 获取 ConfigClient
pub fn get_config_client() -> Option<Arc<nacos_rust_client::client::config_client::ConfigClient>> {
    CONFIG_CLIENT.get().cloned()
}
