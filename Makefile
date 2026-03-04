.PHONY: help build run stop clean logs restart shell

# 默认目标
help:
	@echo "可用命令:"
	@echo "  make build    - 构建 Docker 镜像"
	@echo "  make run      - 启动容器"
	@echo "  make stop     - 停止容器"
	@echo "  make restart  - 重启容器"
	@echo "  make logs     - 查看日志"
	@echo "  make clean    - 清理容器和镜像"
	@echo "  make shell    - 进入容器 shell（调试用）"

# 构建镜像
build:
	docker-compose build

# 启动容器
run:
	docker-compose up -d
	@echo "✅ ms-gateway 已启动在 http://localhost:8080"

# 停止容器
stop:
	docker-compose down

# 重启容器
restart:
	docker-compose restart

# 查看日志
logs:
	docker-compose logs -f ms-gateway

# 清理
clean:
	docker-compose down -v
	docker rmi ms-gateway-ms-gateway 2>/dev/null || true

# 进入容器（distroless 不支持 shell，这里用于调试构建阶段）
shell:
	docker run -it --rm --entrypoint /bin/bash rust:1.83-slim
