# ===== 构建阶段 =====
FROM rust:1.83-slim as builder

# 配置 Cargo 使用国内镜像源（加速构建）
RUN mkdir -p /usr/local/cargo && \
    echo '[source.crates-io]' > /usr/local/cargo/config.toml && \
    echo 'replace-with = "ustc"' >> /usr/local/cargo/config.toml && \
    echo '[source.ustc]' >> /usr/local/cargo/config.toml && \
    echo 'registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"' >> /usr/local/cargo/config.toml

# 安装必要的构建依赖
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# 设置工作目录
WORKDIR /app

# 只复制依赖文件，利用 Docker 缓存
COPY Cargo.toml Cargo.lock ./

# 创建虚拟源文件并构建依赖（缓存层）
RUN mkdir src && \
    echo "fn main() {println!(\"dummy\");}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/deps/ms_gateway*

# 复制实际源代码
COPY src ./src

# 构建真正的应用
RUN cargo build --release --bin ms-gateway

# ===== 运行阶段 =====
FROM gcr.io/distroless/cc-debian12

# 从构建阶段复制二进制文件
COPY --from=builder /app/target/release/ms-gateway /usr/local/bin/ms-gateway

# 复制配置文件
COPY routes.toml /app/routes.toml

# 设置工作目录
WORKDIR /app

# 暴露端口
EXPOSE 8080

# 设置环境变量
ENV RUST_LOG=info
ENV GATEWAY_BIND=0.0.0.0:8080

# 运行应用
ENTRYPOINT ["/usr/local/bin/ms-gateway"]
