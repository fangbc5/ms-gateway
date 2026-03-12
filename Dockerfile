# ===== 构建阶段（Alpine 原生 musl，兼容 arm64/amd64） =====
FROM rust:1.85-alpine AS builder

# 安装构建依赖（vendored openssl 需要 perl、make、gcc）
RUN apk add --no-cache \
    pkgconf \
    perl \
    make \
    musl-dev \
    gcc

# 配置 Cargo 使用国内镜像源（加速构建）
RUN mkdir -p /usr/local/cargo && \
    echo '[source.crates-io]' > /usr/local/cargo/config.toml && \
    echo 'replace-with = "ustc"' >> /usr/local/cargo/config.toml && \
    echo '[source.ustc]' >> /usr/local/cargo/config.toml && \
    echo 'registry = "sparse+https://mirrors.ustc.edu.cn/crates.io-index/"' >> /usr/local/cargo/config.toml

# 设置工作目录
WORKDIR /app

# 只复制依赖文件，利用 Docker 缓存
COPY Cargo.toml Cargo.lock ./

# 创建虚拟源文件并构建依赖（缓存层）
RUN mkdir src && \
    echo "fn main() {println!(\"dummy\");}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/deps/ms_gateway*

# 复制实际源代码和静态资源（include_str! 编译时需要）
COPY src ./src
COPY static ./static

# 构建真正的应用
RUN cargo build --release --bin ms-gateway

# ===== 运行阶段（scratch = 0 字节基础镜像） =====
FROM scratch

# 从构建阶段复制全静态链接的二进制文件
COPY --from=builder /app/target/release/ms-gateway /usr/local/bin/ms-gateway

# 复制 SSL 根证书（HTTPS 请求需要）
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt

# 复制配置文件
COPY routes.toml /app/routes.toml

# 设置工作目录
WORKDIR /app

# 暴露端口
EXPOSE 8080

# 设置环境变量
ENV RUST_LOG=info
ENV GATEWAY_BIND=0.0.0.0:8080
ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt

# 运行应用
ENTRYPOINT ["/usr/local/bin/ms-gateway"]
