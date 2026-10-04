#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Pay3 一键部署与无损升级脚本 (Local & Remote SSH Deployment Script)
# ==============================================================================
# 功能特性:
#   1. 支持本地直接部署 (--mode local，默认不需要输入目录，直接部署当前工程)
#   2. 支持远程 SSH 部署 (--mode ssh / --remote user@host，自动在传入目录下创建 pay3 文件夹部署)
#   3. 支持传入环境变量文件 (--env-file) 与配置文件 (--config-file / pay3.yaml)
#   4. 基于 rsync 进行增量同步，严格排除 data/ 持久化目录与构建缓存
#   5. 重新部署即为升级: 绝不清空历史数据与数据库，自动触发数据库迁移
#   6. 部署后自动验证 /readyz 就绪检查与健康状态
# ==============================================================================

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

# 默认参数
MODE=""
REMOTE_HOST=""
SSH_PORT="22"
SSH_IDENTITY=""
SSH_OPTS=""
TARGET_PARENT_DIR=""
TARGET_DIR=""
ENV_FILE=""
CONFIG_FILE=""
SERVICE_TARGET="pay3"
PREBUILT=false
CN_MIRROR=false
LOCAL_DB=false
COMPOSE_FILE=""
SKIP_HEALTHCHECK=false
DRY_RUN=false
TIMEOUT=90

# 颜色输出
BOLD=$'\033[1m'
GREEN=$'\033[0;32m'
YELLOW=$'\033[1;33m'
RED=$'\033[0;31m'
BLUE=$'\033[0;34m'
NC=$'\033[0m'

log_info() {
  printf "${BLUE}[INFO]${NC} %s\n" "$*"
}

log_success() {
  printf "${GREEN}[SUCCESS]${NC} %s\n" "$*"
}

log_warn() {
  printf "${YELLOW}[WARN]${NC} %s\n" "$*"
}

log_error() {
  printf "${RED}[ERROR]${NC} %s\n" "$*" >&2
}

usage() {
  cat <<EOF
${BOLD}Pay3 自动化部署与无损升级工具${NC}

${BOLD}用法:${NC}
  $0 [选项]

${BOLD}部署服务与形态:${NC}
  --service, --target <pay3|signer|all>
                              指定部署目标服务 (默认: pay3)。
                              * pay3:   仅部署 Pay3 网关主服务 (+ 可选 local-db 数据库)。
                              * signer: 仅部署独立 Signer 签名机服务。
                              * all:    同时部署 Pay3 与 Signer (+ 可选 local-db 数据库)。
  --with-signer               快捷标志，等同于 --service all (同时部署 Pay3 与 Signer)。
  --signer-only               快捷标志，等同于 --service signer (仅部署独立 Signer 签名机)。

${BOLD}部署模式与目标:${NC}
  -m, --mode <local|ssh>      部署模式: local (本地部署) 或 ssh (远程部署)。
                              若指定了 --remote / -s，则默认自动进入 ssh 模式；否则默认为 local。
  -r, --remote, -s, --ssh <host>
                              远程 SSH 主机，格式为 [user@]hostname。指定此项将自动启用 ssh 模式。
  -p, --port <port>           SSH 端口 (默认: 22)。
  -i, --identity <file>       SSH 私钥文件路径 (可选)。
  -d, --dir <dir>             部署父级目录。脚本将在该目录下创建 pay3 文件夹进行部署。
                              * SSH 模式下为必填项 (例如: -d /opt 将部署至 /opt/pay3)。
                              * 本地模式下为可选项，若不填写则默认部署在当前代码仓库根目录。

${BOLD}配置与机密管理:${NC}
  -e, --env-file <file>       环境变量文件路径 (例如: .env.production 或 .env)。
                              同步到目标机后将置为 .env 并设置 600 权限保护。
  -c, --config-file <file>    业务配置文件路径 (例如: pay3.yaml)。
                              同步到目标机后将置为 pay3.yaml 并自动通过 Docker Compose 挂载。

${BOLD}构建与运行选项:${NC}
  --prebuilt                  使用预编译二进制镜像模式 (docker-compose.prebuilt.yml)。
                              若本地缺少编译产物将自动调用 build-prebuilt-binary.sh。
  --cn                        使用中国国内 Docker/Debian 加速镜像 (docker-compose.prebuilt.cn.yml)。
  --local-db                  强制启用内置 PostgreSQL 容器 (--profile local-db)。
                              若未指定，脚本会在检测到 DATABASE_URL 指向 postgres 容器时自动开启。
  --compose-file <file>       指定自定义 Docker Compose 文件 (覆盖默认判断)。
  --skip-healthcheck          部署后跳过健康自检。
  --timeout <seconds>         就绪健康检查超时时间，默认 90 秒。
  --dry-run                   仅打印部署计划与将要执行的命令，不执行真实操作。
  -h, --help                  显示此帮助信息。

${BOLD}使用示例:${NC}
  1. 本地一键部署当前仓库 (默认仅 pay3，免输入目录):
     $0 -e .env.prod -c pay3.prod.yaml

  2. 本地/远程一键一体化全量部署 (Pay3 + Signer + DB 一次性部署完):
     $0 --with-signer -e .env.prod -c pay3.prod.yaml
     # 或远程 SSH 一体化部署:
     $0 --remote root@192.168.1.100 -d /opt --with-signer -e .env.production -c pay3.yaml

  3. 仅部署独立签名机服务 (部署到专用内网机器):
     $0 --remote root@signer.internal -d /opt --signer-only -e .env.signer

  4. 本地指定目录部署/升级:
     $0 --mode local -d /data/services -e .env.prod -c pay3.prod.yaml

  5. 远程 SSH 快速升级 (目标服务器已有配置，仅更新程序版本):
     $0 --remote ubuntu@server.example.com -d /var/www -p 2222
EOF
}

# ------------------------------------------------------------------------------
# 1. 命令行参数解析
# ------------------------------------------------------------------------------
while [[ $# -gt 0 ]]; do
  case "$1" in
    --service|--target)
      SERVICE_TARGET="$2"
      shift 2
      ;;
    --with-signer|--all)
      SERVICE_TARGET="all"
      shift
      ;;
    --signer-only)
      SERVICE_TARGET="signer"
      shift
      ;;
    -m|--mode)
      MODE="$2"
      shift 2
      ;;
    -r|--remote|-s|--ssh)
      REMOTE_HOST="$2"
      shift 2
      ;;
    -p|--port)
      SSH_PORT="$2"
      shift 2
      ;;
    -i|--identity|--identity-file)
      SSH_IDENTITY="$2"
      shift 2
      ;;
    -d|--dir|--target-dir)
      TARGET_PARENT_DIR="$2"
      shift 2
      ;;
    -e|--env-file)
      ENV_FILE="$2"
      shift 2
      ;;
    -c|--config-file)
      CONFIG_FILE="$2"
      shift 2
      ;;
    --prebuilt)
      PREBUILT=true
      shift
      ;;
    --cn)
      CN_MIRROR=true
      shift
      ;;
    --local-db)
      LOCAL_DB=true
      shift
      ;;
    --compose-file)
      COMPOSE_FILE="$2"
      shift 2
      ;;
    --skip-healthcheck)
      SKIP_HEALTHCHECK=true
      shift
      ;;
    --timeout)
      TIMEOUT="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      log_error "未知参数: $1"
      usage
      exit 1
      ;;
  esac
done

# ------------------------------------------------------------------------------
# 2. 模式与目标路径判定
# ------------------------------------------------------------------------------
if [[ -z "$MODE" ]]; then
  if [[ -n "$REMOTE_HOST" ]]; then
    MODE="ssh"
  else
    MODE="local"
  fi
fi

if [[ "$MODE" != "local" && "$MODE" != "ssh" ]]; then
  log_error "不支持的部署模式: $MODE (仅支持 local 或 ssh)"
  exit 1
fi

if [[ "$SERVICE_TARGET" != "pay3" && "$SERVICE_TARGET" != "signer" && "$SERVICE_TARGET" != "all" ]]; then
  log_error "不支持的服务目标: $SERVICE_TARGET (仅支持 pay3, signer 或 all)"
  exit 1
fi

if [[ "$MODE" == "ssh" ]]; then
  if [[ -z "$REMOTE_HOST" ]]; then
    log_error "SSH 模式下必须提供 --remote / -s 参数 (例如: --remote user@host)"
    exit 1
  fi
  if [[ -z "$TARGET_PARENT_DIR" ]]; then
    log_error "SSH 模式下必须提供部署父级目录 -d / --dir (例如: -d /opt，将在 /opt/pay3 部署)"
    exit 1
  fi
  # 处理 pay3 子目录逻辑
  if [[ "$(basename "$TARGET_PARENT_DIR")" == "pay3" ]]; then
    TARGET_DIR="$TARGET_PARENT_DIR"
  else
    TARGET_DIR="${TARGET_PARENT_DIR%/}/pay3"
  fi
else
  # local 模式
  if [[ -z "$TARGET_PARENT_DIR" ]]; then
    TARGET_DIR="$REPO_ROOT"
    log_info "本地部署未指定目录，将默认部署在当前工程目录: $TARGET_DIR"
  else
    if [[ "$(basename "$TARGET_PARENT_DIR")" == "pay3" ]]; then
      TARGET_DIR="$TARGET_PARENT_DIR"
    else
      TARGET_DIR="${TARGET_PARENT_DIR%/}/pay3"
    fi
  fi
fi

# 确定默认使用的 Compose 文件
if [[ -z "$COMPOSE_FILE" ]]; then
  if [[ "$CN_MIRROR" == true ]]; then
    COMPOSE_FILE="docker-compose.prebuilt.cn.yml"
  elif [[ "$PREBUILT" == true ]]; then
    COMPOSE_FILE="docker-compose.prebuilt.yml"
  else
    COMPOSE_FILE="docker-compose.yml"
  fi
fi

# 检查输入文件的有效性
if [[ -n "$ENV_FILE" && ! -f "$ENV_FILE" ]]; then
  log_error "指定的 env 文件不存在: $ENV_FILE"
  exit 1
fi

if [[ -n "$CONFIG_FILE" && ! -f "$CONFIG_FILE" ]]; then
  log_error "指定的配置文件不存在: $CONFIG_FILE"
  exit 1
fi

# ------------------------------------------------------------------------------
# 3. 辅助执行器抽象 (Local / SSH 执行函数)
# ------------------------------------------------------------------------------
SSH_CMD_PREFIX=()
if [[ "$MODE" == "ssh" ]]; then
  SSH_CMD_PREFIX=(ssh -p "$SSH_PORT" -o BatchMode=yes -o StrictHostKeyChecking=accept-new)
  if [[ -n "$SSH_IDENTITY" ]]; then
    SSH_CMD_PREFIX+=(-i "$SSH_IDENTITY")
  fi
  SSH_CMD_PREFIX+=("$REMOTE_HOST")
fi

run_target_sh() {
  local cmd="$1"
  if [[ "$DRY_RUN" == true ]]; then
    if [[ "$MODE" == "ssh" ]]; then
      printf "${YELLOW}[DRY-RUN SSH]${NC} %s '%s'\n" "${SSH_CMD_PREFIX[*]}" "$cmd"
    else
      printf "${YELLOW}[DRY-RUN LOCAL]${NC} (cd '%s' && %s)\n" "$TARGET_DIR" "$cmd"
    fi
    return 0
  fi

  if [[ "$MODE" == "ssh" ]]; then
    "${SSH_CMD_PREFIX[@]}" "bash -c $(printf '%q' "$cmd")"
  else
    (cd "$TARGET_DIR" && bash -c "$cmd")
  fi
}

# ------------------------------------------------------------------------------
# 4. 前置检查与环境验证
# ------------------------------------------------------------------------------
log_info "=== Pay3 部署准备 ==="
log_info "部署模式: $MODE"
log_info "部署目标服务: $SERVICE_TARGET"
log_info "目标部署目录: $TARGET_DIR"
log_info "使用 Compose 文件: $COMPOSE_FILE"
[[ -n "$ENV_FILE" ]] && log_info "指定环境变量文件: $ENV_FILE"
[[ -n "$CONFIG_FILE" ]] && log_info "指定业务配置文件: $CONFIG_FILE"

# 检查本地工具
command -v rsync >/dev/null 2>&1 || { log_error "本地缺少 rsync 工具，请先安装 rsync"; exit 1; }
if [[ "$MODE" == "ssh" ]]; then
  command -v ssh >/dev/null 2>&1 || { log_error "本地缺少 ssh 工具"; exit 1; }
  log_info "测试 SSH 联通性 ($REMOTE_HOST:$SSH_PORT)..."
  if [[ "$DRY_RUN" == false ]]; then
    "${SSH_CMD_PREFIX[@]}" "echo 'SSH connection successful'" >/dev/null || {
      log_error "无法通过 SSH 连接至 $REMOTE_HOST:$SSH_PORT，请检查网络、端口与密钥配置"
      exit 1
    }
  fi
fi

# 如果是 prebuilt 模式且本地缺少二进制，自动触发本地构建
if [[ "$PREBUILT" == true || "$CN_MIRROR" == true ]]; then
  if [[ ! -f "$REPO_ROOT/deploy/prebuilt/pay3" ]]; then
    log_info "检测到预编译模式，但未发现 deploy/prebuilt/pay3，正在本地构建 release 二进制..."
    if [[ "$DRY_RUN" == false ]]; then
      "$REPO_ROOT/scripts/build-prebuilt-binary.sh"
    fi
  fi
fi

# 检查目标机 Docker 与 Compose
log_info "检查目标环境 Docker 与 Docker Compose..."
DETECT_COMPOSE_SCRIPT='
if docker compose version >/dev/null 2>&1; then
  echo "docker compose"
elif command -v docker-compose >/dev/null 2>&1; then
  echo "docker-compose"
else
  echo "NOT_FOUND"
fi
'
if [[ "$DRY_RUN" == false ]]; then
  TARGET_COMPOSE_BIN="$(run_target_sh "$DETECT_COMPOSE_SCRIPT" | tr -d '\r\n')"
  if [[ "$TARGET_COMPOSE_BIN" == *"NOT_FOUND"* || -z "$TARGET_COMPOSE_BIN" ]]; then
    log_error "目标机器上未检测到 Docker Compose (既无 'docker compose' 也无 'docker-compose')！"
    exit 1
  fi
  log_info "目标机器 Docker Compose 命令: $TARGET_COMPOSE_BIN"
else
  TARGET_COMPOSE_BIN="docker compose"
fi

# ------------------------------------------------------------------------------
# 5. 代码与资源同步 (基于 rsync，严格排除 data/，确保不丢数据)
# ------------------------------------------------------------------------------
log_info "正在准备目标目录: $TARGET_DIR ..."
run_target_sh "mkdir -p '$TARGET_DIR'"

log_info "正在使用 rsync 同步项目文件 (自动排除持久化数据目录 data/)..."
RSYNC_SSH_OPTS="ssh -p $SSH_PORT -o BatchMode=yes"
if [[ -n "$SSH_IDENTITY" ]]; then
  RSYNC_SSH_OPTS="$RSYNC_SSH_OPTS -i $SSH_IDENTITY"
fi

# 基础 rsync 排除清单: 绝不排除和清空持久化数据
RSYNC_EXCLUDES=(
  --exclude='.git/'
  --exclude='target/'
  --exclude='data/'          # 核心保护：数据库与挂载数据卷绝不同步删除！
  --exclude='.cargo/'
  --exclude='frontend-test/'
  --exclude='node_modules/'
  --exclude='*.log'
  --exclude='.env*'          # 单独同步指定的 env 文件，避免误覆盖生产机密
  --exclude='pay3*.yaml'     # 单独同步指定的 yaml 配置文件
  --exclude='docker-compose.override.yml'
)

if [[ "$MODE" == "ssh" ]]; then
  TARGET_RSYNC_DEST="${REMOTE_HOST}:${TARGET_DIR}/"
  RSYNC_CMD=(rsync -avz --delete -e "$RSYNC_SSH_OPTS" "${RSYNC_EXCLUDES[@]}" "$REPO_ROOT/" "$TARGET_RSYNC_DEST")
else
  if [[ "$TARGET_DIR" != "$REPO_ROOT" ]]; then
    RSYNC_CMD=(rsync -avz --delete "${RSYNC_EXCLUDES[@]}" "$REPO_ROOT/" "$TARGET_DIR/")
  else
    RSYNC_CMD=(echo "本地目录部署无需自同步代码")
  fi
fi

if [[ "$DRY_RUN" == true ]]; then
  printf "${YELLOW}[DRY-RUN]${NC} %s\n" "${RSYNC_CMD[*]}"
else
  "${RSYNC_CMD[@]}"
fi

# ------------------------------------------------------------------------------
# 6. 配置与环境变量部署
# ------------------------------------------------------------------------------
# 处理 .env 文件
if [[ -n "$ENV_FILE" ]]; then
  log_info "正在同步环境变量配置文件: $ENV_FILE -> $TARGET_DIR/.env ..."
  if [[ "$MODE" == "ssh" ]]; then
    if [[ "$DRY_RUN" == false ]]; then
      rsync -avz -e "$RSYNC_SSH_OPTS" "$ENV_FILE" "${REMOTE_HOST}:${TARGET_DIR}/.env"
      run_target_sh "chmod 600 '$TARGET_DIR/.env'"
    fi
  else
    if [[ "$DRY_RUN" == false ]]; then
      if [[ "$(realpath "$ENV_FILE")" != "$(realpath "$TARGET_DIR/.env" 2>/dev/null || true)" ]]; then
        cp -f "$ENV_FILE" "$TARGET_DIR/.env"
      fi
      chmod 600 "$TARGET_DIR/.env"
    fi
  fi
else
  # 检查目标机是否存在 .env
  CHECK_ENV_SCRIPT="if [ -f '$TARGET_DIR/.env' ]; then echo 'EXISTS'; else echo 'MISSING'; fi"
  if [[ "$DRY_RUN" == false ]]; then
    ENV_EXISTS="$(run_target_sh "$CHECK_ENV_SCRIPT" | tr -d '\r\n')"
    if [[ "$ENV_EXISTS" != *"EXISTS"* ]]; then
      log_warn "目标目录未找到 .env 文件！若服务缺少必要环境变量，启动可能失败。建议通过 -e 传入 .env 文件。"
    else
      log_info "目标目录已存在历史 .env 文件，将直接保留现有配置（升级不破坏配置）。"
    fi
  fi
fi

# 处理 pay3.yaml 业务配置文件
HAS_YAML_CONFIG=false
if [[ -n "$CONFIG_FILE" ]]; then
  log_info "正在同步业务配置文件: $CONFIG_FILE -> $TARGET_DIR/pay3.yaml ..."
  if [[ "$MODE" == "ssh" ]]; then
    if [[ "$DRY_RUN" == false ]]; then
      rsync -avz -e "$RSYNC_SSH_OPTS" "$CONFIG_FILE" "${REMOTE_HOST}:${TARGET_DIR}/pay3.yaml"
      run_target_sh "chmod 644 '$TARGET_DIR/pay3.yaml'"
    fi
  else
    if [[ "$DRY_RUN" == false ]]; then
      if [[ "$(realpath "$CONFIG_FILE")" != "$(realpath "$TARGET_DIR/pay3.yaml" 2>/dev/null || true)" ]]; then
        cp -f "$CONFIG_FILE" "$TARGET_DIR/pay3.yaml"
      fi
      chmod 644 "$TARGET_DIR/pay3.yaml"
    fi
  fi
  HAS_YAML_CONFIG=true
else
  # 检查目标机是否存在 pay3.yaml
  CHECK_YAML_SCRIPT="if [ -f '$TARGET_DIR/pay3.yaml' ]; then echo 'EXISTS'; else echo 'MISSING'; fi"
  if [[ "$DRY_RUN" == false ]]; then
    YAML_EXISTS="$(run_target_sh "$CHECK_YAML_SCRIPT" | tr -d '\r\n')"
    if [[ "$YAML_EXISTS" == *"EXISTS"* ]]; then
      HAS_YAML_CONFIG=true
      log_info "目标目录已存在历史 pay3.yaml 配置文件，将继续挂载使用。"
    fi
  fi
fi

# 处理 Docker Compose YAML 挂载
# 若存在 pay3.yaml，动态生成 docker-compose.override.yml 挂载配置进容器
OVERRIDE_COMPOSE_SCRIPT='
cat << "OVERRIDE_EOF" > docker-compose.override.yml
services:
  pay3:
    volumes:
      - ./pay3.yaml:/var/lib/pay3/pay3.yaml:ro
    environment:
      PAY3_CONFIG_FILE: /var/lib/pay3/pay3.yaml
OVERRIDE_EOF
'
REMOVE_OVERRIDE_SCRIPT='
rm -f docker-compose.override.yml
'

if [[ "$HAS_YAML_CONFIG" == true ]]; then
  log_info "配置 docker-compose.override.yml 以挂载 pay3.yaml 到容器..."
  run_target_sh "cd '$TARGET_DIR' && $OVERRIDE_COMPOSE_SCRIPT"
else
  run_target_sh "cd '$TARGET_DIR' && $REMOVE_OVERRIDE_SCRIPT"
fi

# ------------------------------------------------------------------------------
# 7. 自动化升级与无损启动 (docker compose up -d --build)
# ------------------------------------------------------------------------------
COMPOSE_PROFILES=()
if [[ "$SERVICE_TARGET" == "signer" || "$SERVICE_TARGET" == "all" ]]; then
  COMPOSE_PROFILES+=(--profile signer)
fi

USE_LOCAL_DB=false
if [[ "$SERVICE_TARGET" != "signer" ]]; then
  # 检测是否需要激活 local-db (Compose 内置 PostgreSQL)
  DETECT_LOCAL_DB_SCRIPT='
  if [ -f .env ] && grep -E "DATABASE_URL=.*postgres(ql)?://pay3:[^@]+@postgres:5432" .env >/dev/null 2>&1; then
    echo "LOCAL_DB"
  else
    echo "EXTERNAL_DB"
  fi
  '
  if [[ "$LOCAL_DB" == true ]]; then
    USE_LOCAL_DB=true
  else
    if [[ "$DRY_RUN" == false ]]; then
      DB_DETECTION="$(run_target_sh "cd '$TARGET_DIR' && $DETECT_LOCAL_DB_SCRIPT" | tr -d '\r\n')"
      if [[ "$DB_DETECTION" == *"LOCAL_DB"* ]]; then
        USE_LOCAL_DB=true
        log_info "检测到 .env 中使用本地 Compose 内置数据库连接，将自动启用 --profile local-db"
      fi
    fi
  fi

  if [[ "$USE_LOCAL_DB" == true ]]; then
    COMPOSE_PROFILES+=(--profile local-db)
  fi
fi

if [[ "$SERVICE_TARGET" == "signer" ]]; then
  log_info "=== 开始执行 Signer 独立签名机容器构建与启动 ==="
  DEPLOY_UP_SCRIPT="
cd '$TARGET_DIR'
export PAY3_ENV_FILE=.env
export DATABASE_URL=\${DATABASE_URL:-postgres://placeholder}
$TARGET_COMPOSE_BIN -f '$COMPOSE_FILE' ${COMPOSE_PROFILES[*]} up -d --build signer
"
elif [[ "$SERVICE_TARGET" == "pay3" ]]; then
  log_info "=== 开始执行 Pay3 网关容器重构与无损升级 ==="
  log_info "核心说明: 本操作仅升级应用镜像，原 ./data/postgres 数据库持久卷完整保留，启动后自动执行 Schema 数据库迁移。"
  DEPLOY_UP_SCRIPT="
cd '$TARGET_DIR'
export PAY3_ENV_FILE=.env
$TARGET_COMPOSE_BIN -f '$COMPOSE_FILE' ${COMPOSE_PROFILES[*]} up -d --build pay3 $([[ "$USE_LOCAL_DB" == true ]] && echo "postgres")
"
else
  # all 模式
  log_info "=== 开始执行 Pay3 + Signer (+ 数据库) 一体化全量容器构建与无损升级 ==="
  log_info "核心说明: Signer 与 Pay3 容器联合启动并共享内网，./data/postgres 持久卷完整保留，自动执行 Schema 迁移。"
  DEPLOY_UP_SCRIPT="
cd '$TARGET_DIR'
export PAY3_ENV_FILE=.env
$TARGET_COMPOSE_BIN -f '$COMPOSE_FILE' ${COMPOSE_PROFILES[*]} up -d --build
"
fi

run_target_sh "$DEPLOY_UP_SCRIPT"

# ------------------------------------------------------------------------------
# 8. 服务健康检查与就绪验证
# ------------------------------------------------------------------------------
if [[ "$SKIP_HEALTHCHECK" == true ]]; then
  log_warn "已跳过就绪健康检查 (--skip-healthcheck)"
elif [[ "$DRY_RUN" == true ]]; then
  log_info "[DRY-RUN] 将根据目标服务 ($SERVICE_TARGET) 执行健康自检"
else
  # 1. Signer 健康检查
  if [[ "$SERVICE_TARGET" == "signer" || "$SERVICE_TARGET" == "all" ]]; then
    log_info "正在等待 Signer 签名机服务就绪 (/healthz)..."
    SIGNER_HEALTH_SCRIPT="
cd '$TARGET_DIR'
timeout=$TIMEOUT
start_time=\$(date +%s)
ready=0

while [ \$(( \$(date +%s) - start_time )) -lt \$timeout ]; do
  if $TARGET_COMPOSE_BIN exec -T signer python3 -c \"import urllib.request, os; sys_token=os.getenv('SIGNER_AUTH_TOKEN', ''); req=urllib.request.Request('http://127.0.0.1:8088/healthz'); req.add_header('Authorization', f'Bearer {sys_token}') if sys_token else None; res=urllib.request.urlopen(req); exit(0 if res.getcode()==200 else 1)\" >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 2
done

if [ \$ready -eq 1 ]; then
  echo 'SIGNER_OK'
else
  echo 'SIGNER_FAILED'
  echo '--- signer 容器尾部日志 ---'
  $TARGET_COMPOSE_BIN logs --tail=60 signer
fi
"
    CHECK_OUTPUT="$(run_target_sh "$SIGNER_HEALTH_SCRIPT")"
    if [[ "$CHECK_OUTPUT" == *"SIGNER_OK"* ]]; then
      log_success "Signer 签名机启动成功并通过健康检查 (/healthz OK)！"
    else
      log_error "Signer 签名机健康检查未通过！详细日志输出如下:"
      printf "%s\n" "$CHECK_OUTPUT"
      exit 1
    fi
  fi

  # 2. Pay3 网关深度健康检查
  if [[ "$SERVICE_TARGET" == "pay3" || "$SERVICE_TARGET" == "all" ]]; then
    log_info "正在等待 Pay3 服务就绪并执行健康检查 (/readyz)..."
    PAY3_HEALTH_SCRIPT="
cd '$TARGET_DIR'
timeout=$TIMEOUT
start_time=\$(date +%s)
ready=0

while [ \$(( \$(date +%s) - start_time )) -lt \$timeout ]; do
  if $TARGET_COMPOSE_BIN exec -T pay3 curl -fsS http://127.0.0.1:3000/readyz >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 3
done

if [ \$ready -eq 1 ]; then
  echo 'PAY3_OK'
else
  echo 'PAY3_FAILED'
  echo '--- pay3 容器尾部日志 ---'
  $TARGET_COMPOSE_BIN logs --tail=60 pay3
fi
"
    CHECK_OUTPUT="$(run_target_sh "$PAY3_HEALTH_SCRIPT")"
    if [[ "$CHECK_OUTPUT" == *"PAY3_OK"* ]]; then
      log_success "Pay3 服务启动成功并通过就绪健康检查 (/readyz OK)！"
    else
      log_error "Pay3 服务健康检查超时或未通过！详细日志输出如下:"
      printf "%s\n" "$CHECK_OUTPUT"
      exit 1
    fi
  fi
fi

# ------------------------------------------------------------------------------
# 9. 部署完成状态汇报
# ------------------------------------------------------------------------------
SHOW_STATUS_SCRIPT="
cd '$TARGET_DIR'
$TARGET_COMPOSE_BIN ps
"
log_info "=== 当前服务容器状态 ==="
run_target_sh "$SHOW_STATUS_SCRIPT"

log_success "=== 恭喜！Pay3 部署与升级已成功完成 ==="
log_info "部署目录: $TARGET_DIR"
log_info "常用运维命令:"
if [[ "$MODE" == "ssh" ]]; then
  printf "  - 查看服务日志: ssh -p %s %s \"cd '%s' && %s logs -f pay3\"\n" "$SSH_PORT" "$REMOTE_HOST" "$TARGET_DIR" "$TARGET_COMPOSE_BIN"
  printf "  - 查看容器状态: ssh -p %s %s \"cd '%s' && %s ps\"\n" "$SSH_PORT" "$REMOTE_HOST" "$TARGET_DIR" "$TARGET_COMPOSE_BIN"
else
  printf "  - 查看服务日志: cd '%s' && %s logs -f pay3\n" "$TARGET_DIR" "$TARGET_COMPOSE_BIN"
  printf "  - 查看容器状态: cd '%s' && %s ps\n" "$TARGET_DIR" "$TARGET_COMPOSE_BIN"
fi
