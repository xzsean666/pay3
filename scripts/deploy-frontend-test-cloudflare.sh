#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
FRONTEND_ROOT="${REPO_ROOT}/frontend-test"

ENV_FILE="${PAY3_FRONTEND_ENV_FILE:-${REPO_ROOT}/.env.test}"
API_BASE_URL=""

usage() {
  cat <<'USAGE'
Usage:
  scripts/deploy-frontend-test-cloudflare.sh --api-base-url https://pay3-api.example [options]

Options:
  --api-base-url <url>    Required. Public Pay3 API URL Cloudflare can reach.
  --project-name <name>   Cloudflare Pages project name. Default: pay3-frontend-test.
  --branch <name>         Pages deployment branch.
  --env-file <path>       Env file for JWT config. Default: .env.test.
  --dry-run               Print Pages deploy actions without uploading secrets or deploying.
  --no-create-project     Do not create the Cloudflare Pages project when missing.
  --ttl-seconds <n>       Generated JWT TTL. Default: 604800.
  --subject <sub>         JWT sub. Default: frontend-test.
  --scopes <scopes>       JWT scopes. Default: orders:create orders:read orders:verify.
  --no-direct             Disable browser-direct mode.
  --hide-token            Do not expose generated JWT to browser config.
  --default-mode <mode>   proxy or direct. Default: proxy.
  --allow-local-api       Allow localhost/127.0.0.1 API URL for tunnel tests.
  --keep-vars             Legacy worker-assets flag retained for compatibility.
  -h, --help              Show this help.
USAGE
}

args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --)
      shift
      ;;
    --api-base-url)
      [[ $# -ge 2 ]] || { echo "error: --api-base-url requires a value" >&2; exit 1; }
      API_BASE_URL="$2"
      args+=("$1" "$2")
      shift 2
      ;;
    --env-file)
      [[ $# -ge 2 ]] || { echo "error: --env-file requires a value" >&2; exit 1; }
      ENV_FILE="$2"
      args+=("$1" "$2")
      shift 2
      ;;
    --project-name|--branch)
      [[ $# -ge 2 ]] || { echo "error: $1 requires a value" >&2; exit 1; }
      args+=("$1" "$2")
      shift 2
      ;;
    --dry-run|--no-direct|--hide-token|--direct|--expose-token|--allow-local-api|--keep-vars|--create-project|--no-create-project)
      args+=("$1")
      shift
      ;;
    --ttl-seconds|--subject|--scopes|--default-mode)
      [[ $# -ge 2 ]] || { echo "error: $1 requires a value" >&2; exit 1; }
      args+=("$1" "$2")
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "error: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "${API_BASE_URL}" && -z "${PAY3_FRONTEND_API_BASE_URL:-}" && -z "${PAY3_API_BASE_URL:-}" ]]; then
  echo "error: pass --api-base-url https://your-pay3-api.example" >&2
  exit 1
fi

if [[ ! -f "${ENV_FILE}" ]]; then
  echo "error: env file not found: ${ENV_FILE}" >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "error: node is required" >&2
  exit 1
fi

if ! command -v pnpm >/dev/null 2>&1; then
  echo "error: pnpm is required" >&2
  exit 1
fi

exec node "${FRONTEND_ROOT}/scripts/deploy-wrangler-page.mjs" --target pages --env-file "${ENV_FILE}" "${args[@]}"
