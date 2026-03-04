# Docker 部署指南

## 快速开始

### 一键部署
```bash
./deploy.sh
```

### 使用 Makefile
```bash
# 构建镜像
make build

# 启动服务
make run

# 查看日志
make logs

# 停止服务
make stop

# 重启服务
make restart

# 清理所有
make clean
```

### 使用 docker-compose
```bash
# 构建并启动
docker-compose up -d

# 查看日志
docker-compose logs -f

# 停止服务
docker-compose down
```

## 镜像特点

- **多阶段构建**：分离构建和运行环境
- **最小化镜像**：使用 distroless 基础镜像，最终镜像约 20-30MB
- **安全性**：无 shell、无包管理器，减少攻击面
- **依赖缓存**：优化构建速度

## 配置

### 环境变量

在 `docker-compose.yml` 中配置：

```yaml
environment:
  - RUST_LOG=info              # 日志级别
  - GATEWAY_BIND=0.0.0.0:8080  # 监听地址
  - JWT_SECRET=your-secret     # JWT 密钥
```

### 配置文件

修改 `routes.toml` 后重启容器：

```bash
docker-compose restart
```

## 生产部署建议

### 1. 使用环境变量文件

创建 `.env` 文件：
```bash
JWT_SECRET=your-production-secret-key
RUST_LOG=warn
```

### 2. 持久化日志

```yaml
volumes:
  - ./logs:/app/logs
```

### 3. 资源限制

```yaml
deploy:
  resources:
    limits:
      cpus: '1'
      memory: 512M
    reservations:
      cpus: '0.5'
      memory: 256M
```

### 4. 健康检查

已内置健康检查，可通过以下命令查看：
```bash
docker inspect ms-gateway | grep -A 10 Health
```

## 故障排查

### 查看日志
```bash
docker-compose logs -f ms-gateway
```

### 检查容器状态
```bash
docker-compose ps
```

### 进入容器（调试）
distroless 镜像不包含 shell，如需调试，修改 Dockerfile 使用 debian-slim：
```dockerfile
FROM debian:12-slim
```

## 镜像大小对比

- **rust:1.83-slim**（构建阶段）：~1.5GB
- **distroless/cc-debian12**（运行阶段）：~20MB
- **最终镜像**：~25-35MB（含二进制文件）

## 网络配置

容器默认使用桥接网络 `gateway-network`，如需连接其他服务：

```yaml
services:
  ms-gateway:
    networks:
      - gateway-network
      - backend-network
```
