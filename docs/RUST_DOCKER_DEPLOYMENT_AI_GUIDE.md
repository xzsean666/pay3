# Rust 项目 Docker 部署生成指南

这是一份通用文档。复制到任意 Rust 服务项目后，AI 应按本文档为该项目生成 Docker 部署文件。

目标是同时支持三种部署方式：

1. 普通 Docker build：Docker build 内部编译 Rust 源码。
2. 预编译 binary 轻量部署：先在宿主机或 CI 编译 binary，Docker 只打包运行环境。
3. 中国网络优化部署：基于预编译 binary，并支持替换 Docker 基础镜像和 Debian apt 源。

本文档中的占位符必须按目标项目实际情况替换：

| 占位符 | 含义 | 示例 |
| --- | --- | --- |
| `<APP_NAME>` | 应用名、容器用户、数据目录名 | `myapp` |
| `<BIN_NAME>` | Rust 编译出的可执行文件名 | `myapp` |
| `<ENV_PREFIX>` | 环境变量前缀，大写 | `MYAPP` |
| `<PORT>` | 容器内监听端口 | `3000` |
| `<BIND_ENV>` | 应用监听地址环境变量 | `APP_BIND` |
| `<HEALTH_PATH>` | health 或 readiness 路径 | `/healthz` 或 `/readyz` |
| `<DATA_DIR>` | 容器内持久化目录 | `/var/lib/myapp` |
| `<RUST_VERSION>` | Rust 镜像版本 | `1.91` |

如果目标项目没有持久化数据目录，可以去掉 `VOLUME` 和 compose volume；如果没有 HTTP health endpoint，应先在应用里补一个最小 `/healthz`。

## AI 执行流程

AI 拿到这份文档后，必须按顺序做以下事情。

1. 读取 `Cargo.toml`，确认 package name、workspace 结构、是否存在多个 `[[bin]]`。
2. 确认实际要部署的 binary 名称。如果没有多个 binary，通常就是 package name。
3. 查找应用监听端口、监听地址环境变量、health endpoint、需要的 runtime env。
4. 查找运行时是否需要额外文件，例如 `migrations`、`static`、`templates`、`assets`、配置目录。
5. 查找运行时是否需要额外系统包，例如 `libssl3`、`libpq5`、`tzdata`。
6. 生成普通 `Dockerfile`。
7. 生成 `scripts/build-prebuilt-binary.sh`。
8. 生成 `Dockerfile.prebuilt`。
9. 生成 `Dockerfile.prebuilt.cn`。
10. 生成 `docker-compose.yml`、`docker-compose.prebuilt.yml`、`docker-compose.prebuilt.cn.yml`。
11. 生成或更新 `.dockerignore`。
12. 生成 `deploy/prebuilt/README.md`。
13. 更新 `.gitignore`，忽略预编译 binary。
14. 运行必要的格式、构建或 compose config 校验。

AI 不应该把本文档里的示例项目名原样复制到结果文件中。所有 `<...>` 占位符都必须替换成目标项目的真实值。

## 推荐文件结构

生成后项目根目录建议包含：

```text
.
├── Dockerfile
├── Dockerfile.prebuilt
├── Dockerfile.prebuilt.cn
├── docker-compose.yml
├── docker-compose.prebuilt.yml
├── docker-compose.prebuilt.cn.yml
├── .dockerignore
├── deploy/
│   └── prebuilt/
│       └── README.md
└── scripts/
    └── build-prebuilt-binary.sh
```

## 方式一：普通 Docker Build

适合场景：

- 本地开发或 CI 标准构建。
- Docker build 机器可以正常访问 Rust 镜像、Cargo registry 和 apt 源。
- 希望镜像构建过程完全从源码开始，方便复现。

生成 `Dockerfile`：

```Dockerfile
# syntax=docker/dockerfile:1

FROM rust:<RUST_VERSION>-bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release --locked && \
    cp /app/target/release/<BIN_NAME> /usr/local/bin/<BIN_NAME>

FROM debian:bookworm-slim AS runtime

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 <APP_NAME> && \
    useradd --system --uid 10001 --gid <APP_NAME> --home-dir <DATA_DIR> <APP_NAME> && \
    mkdir -p <DATA_DIR> && \
    chown -R <APP_NAME>:<APP_NAME> <DATA_DIR>

COPY --from=builder /usr/local/bin/<BIN_NAME> /usr/local/bin/<BIN_NAME>

ENV <BIND_ENV>=0.0.0.0:<PORT>

WORKDIR <DATA_DIR>
USER <APP_NAME>:<APP_NAME>

EXPOSE <PORT>
VOLUME ["<DATA_DIR>"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:<PORT><HEALTH_PATH> >/dev/null || exit 1

ENTRYPOINT ["<BIN_NAME>"]
```

如果项目是 workspace，不能只复制 `src`，需要复制 workspace 成员目录。例如：

```Dockerfile
COPY crates ./crates
COPY apps ./apps
```

如果项目运行时需要 migrations 或静态资源，必须复制：

```Dockerfile
COPY migrations ./migrations
COPY static ./static
```

如果 `sqlx::migrate!()`、`include_str!()`、`include_bytes!()` 在编译期读取文件，builder 阶段也必须复制这些文件。

`RUN --mount=type=cache` 依赖 BuildKit。现代 Docker 通常默认可用；旧环境需要启用 BuildKit，或者去掉 cache mount。

普通 build 命令：

```bash
docker build -f Dockerfile -t <APP_NAME>:local .
```

## 方式二：预编译 Binary 轻量部署

适合场景：

- CI 或宿主机先完成 Rust release build。
- 服务器只负责打包和运行，不下载 Cargo 依赖。
- 需要更快、更轻的 Docker build。
- 中国大陆服务器部署前，希望先绕开 Cargo 网络问题。

### build 脚本

生成 `scripts/build-prebuilt-binary.sh`：

```bash
#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

binary_name="<BIN_NAME>"
output_path="${<ENV_PREFIX>_PREBUILT_BINARY:-deploy/prebuilt/<BIN_NAME>}"
cargo_target_dir="${CARGO_TARGET_DIR:-target}"
cargo_args=(build --release --locked)

if [[ -n "${<ENV_PREFIX>_CARGO_TARGET:-}" ]]; then
  cargo_args+=(--target "$<ENV_PREFIX>_CARGO_TARGET")
  built_binary="$cargo_target_dir/$<ENV_PREFIX>_CARGO_TARGET/release/$binary_name"
else
  built_binary="$cargo_target_dir/release/$binary_name"
fi

cargo "${cargo_args[@]}"

install -d -m 0755 "$(dirname "$output_path")"
install -m 0755 "$built_binary" "$output_path"

printf 'Built prebuilt Docker binary: %s\n' "$output_path"
```

生成后设置可执行权限：

```bash
chmod +x scripts/build-prebuilt-binary.sh
```

使用方式：

```bash
scripts/build-prebuilt-binary.sh
```

可选环境变量：

| 变量 | 作用 |
| --- | --- |
| `<ENV_PREFIX>_PREBUILT_BINARY` | 覆盖输出 binary 路径，默认 `deploy/prebuilt/<BIN_NAME>`。 |
| `<ENV_PREFIX>_CARGO_TARGET` | 传给 `cargo build --target`。 |
| `CARGO_TARGET_DIR` | 覆盖 Cargo target 目录。 |

### 预编译 Dockerfile

生成 `Dockerfile.prebuilt`：

```Dockerfile
# syntax=docker/dockerfile:1

FROM debian:bookworm-slim AS runtime

ARG <ENV_PREFIX>_BINARY=deploy/prebuilt/<BIN_NAME>

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --system --gid 10001 <APP_NAME> && \
    useradd --system --uid 10001 --gid <APP_NAME> --home-dir <DATA_DIR> <APP_NAME> && \
    mkdir -p <DATA_DIR> && \
    chown -R <APP_NAME>:<APP_NAME> <DATA_DIR>

COPY ${<ENV_PREFIX>_BINARY} /usr/local/bin/<BIN_NAME>
RUN chmod 0755 /usr/local/bin/<BIN_NAME>

ENV <BIND_ENV>=0.0.0.0:<PORT>

WORKDIR <DATA_DIR>
USER <APP_NAME>:<APP_NAME>

EXPOSE <PORT>
VOLUME ["<DATA_DIR>"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:<PORT><HEALTH_PATH> >/dev/null || exit 1

ENTRYPOINT ["<BIN_NAME>"]
```

构建命令：

```bash
scripts/build-prebuilt-binary.sh
docker build -f Dockerfile.prebuilt -t <APP_NAME>:prebuilt .
```

注意：

- 预编译 binary 必须匹配容器 runtime 的 OS、CPU 架构和 libc。
- 在 macOS 或 Windows 上直接编译出来的 binary 不能放进 Linux Debian 容器运行。
- 如果 runtime 使用 Debian slim，通常需要 Linux glibc binary。
- 如果改成 musl 静态链接，runtime 可以进一步简化，但本文默认使用 Debian slim。

## 方式三：中国网络优化部署

适合场景：

- 服务器在中国大陆。
- Docker Hub、Debian apt 源、Cargo registry 访问不稳定。
- 希望基础镜像和 apt 源都可以通过环境变量替换。

中国优化方案基于预编译 binary。先在网络更稳定的机器或 CI 中构建 binary，再在服务器上打包 runtime 镜像。

生成 `Dockerfile.prebuilt.cn`：

```Dockerfile
# syntax=docker/dockerfile:1

ARG DEBIAN_IMAGE=m.daocloud.io/docker.io/library/debian:bookworm-slim
FROM ${DEBIAN_IMAGE} AS runtime

ARG <ENV_PREFIX>_BINARY=deploy/prebuilt/<BIN_NAME>
ARG DEBIAN_MIRROR=http://mirrors.aliyun.com/debian
ARG DEBIAN_SECURITY_MIRROR=http://mirrors.aliyun.com/debian-security

RUN set -eux; \
    sources_file="/etc/apt/sources.list.d/debian.sources"; \
    if [ ! -f "$sources_file" ]; then sources_file="/etc/apt/sources.list"; fi; \
    sed -i \
        -e "s#http://deb.debian.org/debian#${DEBIAN_MIRROR}#g" \
        -e "s#http://deb.debian.org/debian-security#${DEBIAN_SECURITY_MIRROR}#g" \
        -e "s#http://security.debian.org/debian-security#${DEBIAN_SECURITY_MIRROR}#g" \
        "$sources_file"; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl; \
    rm -rf /var/lib/apt/lists/*; \
    groupadd --system --gid 10001 <APP_NAME>; \
    useradd --system --uid 10001 --gid <APP_NAME> --home-dir <DATA_DIR> <APP_NAME>; \
    mkdir -p <DATA_DIR>; \
    chown -R <APP_NAME>:<APP_NAME> <DATA_DIR>

COPY ${<ENV_PREFIX>_BINARY} /usr/local/bin/<BIN_NAME>
RUN chmod 0755 /usr/local/bin/<BIN_NAME>

ENV <BIND_ENV>=0.0.0.0:<PORT>

WORKDIR <DATA_DIR>
USER <APP_NAME>:<APP_NAME>

EXPOSE <PORT>
VOLUME ["<DATA_DIR>"]

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD curl -fsS http://127.0.0.1:<PORT><HEALTH_PATH> >/dev/null || exit 1

ENTRYPOINT ["<BIN_NAME>"]
```

构建命令：

```bash
scripts/build-prebuilt-binary.sh

docker build \
  -f Dockerfile.prebuilt.cn \
  --build-arg <ENV_PREFIX>_BINARY=deploy/prebuilt/<BIN_NAME> \
  --build-arg DEBIAN_IMAGE=m.daocloud.io/docker.io/library/debian:bookworm-slim \
  --build-arg DEBIAN_MIRROR=http://mirrors.aliyun.com/debian \
  --build-arg DEBIAN_SECURITY_MIRROR=http://mirrors.aliyun.com/debian-security \
  -t <APP_NAME>:prebuilt-cn \
  .
```

## Docker Compose 模板

### 普通源码构建

生成 `docker-compose.yml`：

```yaml
services:
  app:
    build:
      context: .
      dockerfile: Dockerfile
    image: <APP_NAME>:local
    restart: unless-stopped
    env_file:
      - ${<ENV_PREFIX>_ENV_FILE:-.env.example}
    environment:
      <BIND_ENV>: 0.0.0.0:<PORT>
    ports:
      - "${<ENV_PREFIX>_PORT:-<PORT>}:<PORT>"
    volumes:
      - app-data:<DATA_DIR>

volumes:
  app-data:
```

启动：

```bash
<ENV_PREFIX>_ENV_FILE=.env docker compose --env-file .env up -d --build app
```

### 预编译 binary

生成 `docker-compose.prebuilt.yml`：

```yaml
services:
  app:
    build:
      context: .
      dockerfile: Dockerfile.prebuilt
      args:
        <ENV_PREFIX>_BINARY: ${<ENV_PREFIX>_PREBUILT_BINARY:-deploy/prebuilt/<BIN_NAME>}
    image: <APP_NAME>:prebuilt
    restart: unless-stopped
    env_file:
      - ${<ENV_PREFIX>_ENV_FILE:-.env.example}
    environment:
      <BIND_ENV>: 0.0.0.0:<PORT>
    ports:
      - "${<ENV_PREFIX>_PORT:-<PORT>}:<PORT>"
    volumes:
      - app-data:<DATA_DIR>

volumes:
  app-data:
```

启动：

```bash
scripts/build-prebuilt-binary.sh

<ENV_PREFIX>_ENV_FILE=.env docker compose \
  --env-file .env \
  -f docker-compose.prebuilt.yml \
  up -d --build app
```

### 中国网络优化预编译

生成 `docker-compose.prebuilt.cn.yml`：

```yaml
services:
  app:
    build:
      context: .
      dockerfile: Dockerfile.prebuilt.cn
      args:
        <ENV_PREFIX>_BINARY: ${<ENV_PREFIX>_PREBUILT_BINARY:-deploy/prebuilt/<BIN_NAME>}
        DEBIAN_IMAGE: ${<ENV_PREFIX>_DEBIAN_IMAGE:-m.daocloud.io/docker.io/library/debian:bookworm-slim}
        DEBIAN_MIRROR: ${<ENV_PREFIX>_DEBIAN_MIRROR:-http://mirrors.aliyun.com/debian}
        DEBIAN_SECURITY_MIRROR: ${<ENV_PREFIX>_DEBIAN_SECURITY_MIRROR:-http://mirrors.aliyun.com/debian-security}
    image: <APP_NAME>:prebuilt-cn
    restart: unless-stopped
    env_file:
      - ${<ENV_PREFIX>_ENV_FILE:-.env.example}
    environment:
      <BIND_ENV>: 0.0.0.0:<PORT>
    ports:
      - "${<ENV_PREFIX>_PORT:-<PORT>}:<PORT>"
    volumes:
      - app-data:<DATA_DIR>

volumes:
  app-data:
```

启动：

```bash
scripts/build-prebuilt-binary.sh

<ENV_PREFIX>_ENV_FILE=.env docker compose \
  --env-file .env \
  -f docker-compose.prebuilt.cn.yml \
  up -d --build app
```

可覆盖的中国优化变量：

| 变量 | 默认值 | 作用 |
| --- | --- | --- |
| `<ENV_PREFIX>_DEBIAN_IMAGE` | `m.daocloud.io/docker.io/library/debian:bookworm-slim` | Debian runtime 基础镜像。 |
| `<ENV_PREFIX>_DEBIAN_MIRROR` | `http://mirrors.aliyun.com/debian` | Debian apt 主源。 |
| `<ENV_PREFIX>_DEBIAN_SECURITY_MIRROR` | `http://mirrors.aliyun.com/debian-security` | Debian security apt 源。 |
| `<ENV_PREFIX>_PREBUILT_BINARY` | `deploy/prebuilt/<BIN_NAME>` | 预编译 binary 路径。 |

如果 compose 中还有 PostgreSQL、Redis、Nginx 等依赖服务，也应为它们提供可替换镜像变量，例如：

```yaml
services:
  postgres:
    image: ${<ENV_PREFIX>_POSTGRES_IMAGE:-m.daocloud.io/docker.io/library/postgres:16-alpine}
```

## env 文件规则

建议提供 `.env.example`，至少包含：

```dotenv
<ENV_PREFIX>_PORT=<PORT>
<BIND_ENV>=0.0.0.0:<PORT>
```

如果项目需要数据库、Redis、JWT、RPC、对象存储等配置，也放示例 key，但不要放真实 secret。

使用非默认 env 文件时，命令里要同时传两处：

```bash
<ENV_PREFIX>_ENV_FILE=.env.staging docker compose --env-file .env.staging up -d --build app
```

原因：

- `--env-file` 用于 compose 变量插值，例如 `${<ENV_PREFIX>_PORT}`。
- service 的 `env_file:` 用于把变量注入容器。

## dockerignore

生成或更新根目录 `.dockerignore`：

```dockerignore
.git
.gitignore
target
cache
out
data
tmp
*.log
.env
.env.*
!.env.example
```

如果使用 Dockerfile 专属 ignore 文件，可生成 `Dockerfile.prebuilt.dockerignore`：

```dockerignore
**
!deploy/
!deploy/prebuilt/
!deploy/prebuilt/**
!Dockerfile.prebuilt
```

以及 `Dockerfile.prebuilt.cn.dockerignore`：

```dockerignore
**
!deploy/
!deploy/prebuilt/
!deploy/prebuilt/**
!Dockerfile.prebuilt.cn
```

现代 Docker/BuildKit 支持 Dockerfile 专属 ignore 文件，例如 `Dockerfile.prebuilt.dockerignore`。如果目标构建工具不支持该特性，则会退回根目录 `.dockerignore`。

## gitignore

预编译 binary 通常不应该提交到 git。更新 `.gitignore`：

```gitignore
/deploy/prebuilt/*
!/deploy/prebuilt/README.md
```

生成 `deploy/prebuilt/README.md`：

````markdown
# Prebuilt Binary

This directory is used as Docker build context input for `Dockerfile.prebuilt`
and `Dockerfile.prebuilt.cn`.

Generate the binary with:

```bash
scripts/build-prebuilt-binary.sh
```

The generated binary is intentionally ignored by git.
````

## 验证命令

生成文件后，至少执行：

```bash
cargo build --release --locked
scripts/build-prebuilt-binary.sh
docker build -f Dockerfile -t <APP_NAME>:local .
docker build -f Dockerfile.prebuilt -t <APP_NAME>:prebuilt .
docker build -f Dockerfile.prebuilt.cn -t <APP_NAME>:prebuilt-cn .
docker compose config
docker compose -f docker-compose.prebuilt.yml config
docker compose -f docker-compose.prebuilt.cn.yml config
```

启动验证：

```bash
<ENV_PREFIX>_ENV_FILE=.env docker compose --env-file .env up -d --build app
docker compose --env-file .env ps
docker compose --env-file .env logs -f app
curl -fsS http://127.0.0.1:<PORT><HEALTH_PATH>
```

预编译验证：

```bash
scripts/build-prebuilt-binary.sh

<ENV_PREFIX>_ENV_FILE=.env docker compose \
  --env-file .env \
  -f docker-compose.prebuilt.yml \
  up -d --build app
```

中国优化验证：

```bash
scripts/build-prebuilt-binary.sh

<ENV_PREFIX>_ENV_FILE=.env docker compose \
  --env-file .env \
  -f docker-compose.prebuilt.cn.yml \
  up -d --build app
```

## 常见问题

### binary 在容器里无法运行

排查：

```bash
file deploy/prebuilt/<BIN_NAME>
docker run --rm -it --entrypoint /bin/sh <APP_NAME>:prebuilt
ldd /usr/local/bin/<BIN_NAME>
```

常见原因：

- 在 macOS 或 Windows 上构建了非 Linux binary。
- arm64 binary 被放进 amd64 容器，或反过来。
- glibc/musl 不匹配。
- runtime 镜像缺少动态链接库。

### healthcheck 一直失败

检查：

- 应用是否监听 `0.0.0.0:<PORT>`。
- 容器内端口、`EXPOSE`、compose `ports` 是否一致。
- `<HEALTH_PATH>` 是否真实存在。
- 如果使用 `/readyz`，它可能依赖数据库、外部服务或队列；依赖未满足时失败是正常的。

### compose 变量为空

检查：

- 变量是否在 shell、项目根 `.env` 或 `docker compose --env-file` 指定文件中。
- service 内部需要的变量是否同时通过 `env_file:` 或 `environment:` 注入容器。

### 国内构建仍然慢

检查：

- 是否使用 `Dockerfile.prebuilt.cn`。
- 是否已经提前执行 `scripts/build-prebuilt-binary.sh`。
- `<ENV_PREFIX>_DEBIAN_IMAGE` 是否可访问。
- apt mirror 是否可访问。
- compose 里的其他依赖镜像是否也替换为国内可访问镜像。

## 选择建议

| 场景 | 推荐方式 |
| --- | --- |
| CI 标准构建、本地开发 | 普通 Docker build |
| 服务器不想安装 Rust 或拉 Cargo 依赖 | 预编译 binary |
| 中国大陆服务器部署 | 中国网络优化预编译 |
| 需要最快镜像打包 | 预编译 binary |
| 需要构建过程最容易复现 | 普通 Docker build |

默认建议三个入口都生成：普通 `Dockerfile` 保证标准路径，`Dockerfile.prebuilt` 保证轻量部署，`Dockerfile.prebuilt.cn` 解决国内网络环境。
