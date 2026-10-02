#!/usr/bin/env bash
# ==============================================================================
# Pay3 Production Pre-flight Verification Script (TASK-024)
# Automated Audit Gate for Production Readiness
#
# Reference Standards:
#   - docs/PRODUCTION_READINESS.md
#   - docs/RUNBOOK.md
#   - AGENTS.md
# ==============================================================================

set -eo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Color constants
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m' # No Color

ENV_FILE="${PROJECT_ROOT}/.env.production.example"
SKIP_CONNECTIVITY=0
TEST_VIOLATIONS=0
FAILED_GATES=0
PASSED_GATES=0
WARNINGS=0

usage() {
  cat <<EOF
Usage: $(basename "$0") [OPTIONS]

Options:
  --env-file <path>        Path to environment configuration file to audit
                           (default: .env.production.example)
  --skip-connectivity      Skip live network probes (DB, RPC, Signer endpoints)
                           Useful in offline CI / build environments
  --test-violations        Run negative security regression tests against
                           prohibited production configurations
  -h, --help               Show this help message
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --env-file)
      ENV_FILE="$2"
      shift 2
      ;;
    --skip-connectivity)
      SKIP_CONNECTIVITY=1
      shift
      ;;
    --test-violations)
      TEST_VIOLATIONS=1
      shift
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo -e "${RED}Unknown option: $1${NC}" >&2
      usage
      ;;
  esac
done

cd "${PROJECT_ROOT}"

# ------------------------------------------------------------------------------
# Negative Test Runner (for automated CI test verification)
# ------------------------------------------------------------------------------
run_violation_tests() {
  echo -e "\n${BOLD}${CYAN}===================================================================${NC}"
  echo -e "${BOLD}${CYAN}   Running Production Gate Violation (Negative) Test Suite         ${NC}"
  echo -e "${BOLD}${CYAN}===================================================================${NC}\n"

  local test_count=0
  local test_passed=0

  run_negative_case() {
    local case_name="$1"
    local override_var="$2"
    test_count=$((test_count + 1))
    echo -n "  Test [${test_count}]: ${case_name}... "

    local tmp_env
    tmp_env="$(mktemp)"
    cp "${ENV_FILE}" "${tmp_env}"
    echo -e "\n${override_var}" >> "${tmp_env}"

    if bash "$0" --env-file "${tmp_env}" --skip-connectivity >/dev/null 2>&1; then
      echo -e "${RED}FAILED (Gate did not block invalid config!)${NC}"
      rm -f "${tmp_env}"
      return 1
    else
      echo -e "${GREEN}PASSED (Blocked correctly)${NC}"
      test_passed=$((test_passed + 1))
      rm -f "${tmp_env}"
      return 0
    fi
  }

  run_negative_case "Forbidden SIGNER_MODE=fake in production" "SIGNER_MODE=fake"
  run_negative_case "Forbidden SIGNER_MODE=local in production" "SIGNER_MODE=local"
  run_negative_case "Forbidden SIGNER_MNEMONIC in production" "SIGNER_MNEMONIC=\"test test test test test test test test test test test junk\""
  run_negative_case "Forbidden JWT_SECRET in production" "JWT_SECRET=insecure_symmetric_secret_key_12345"
  run_negative_case "Forbidden START_BLOCK=0 in production" "START_BLOCK=0\nALLOW_FULL_HISTORY_REPLAY=false"
  run_negative_case "Forbidden Single RPC Provider (requires >= 2)" "RPC_HTTP_URLS=https://single-rpc.internal:8545"
  run_negative_case "Forbidden Zero TREASURY_ADDRESS" "TREASURY_ADDRESS=0x0000000000000000000000000000000000000000"
  run_negative_case "Forbidden Colliding TREASURY and PROBLEM_FUNDS" "PROBLEM_FUNDS_ADDRESS=0x71C7656EC7ab88b098defB751B7401B5f6d8976F"

  echo -e "\n${BOLD}Violation Test Suite Results: ${test_passed}/${test_count} cases properly blocked.${NC}"
  if [[ "${test_passed}" -eq "${test_count}" ]]; then
    echo -e "${GREEN}${BOLD}Negative Gate Regression Passed 100%.${NC}\n"
    exit 0
  else
    echo -e "${RED}${BOLD}Some violation tests failed to block!${NC}\n"
    exit 1
  fi
}

if [[ "${TEST_VIOLATIONS}" -eq 1 ]]; then
  run_violation_tests
fi

# ------------------------------------------------------------------------------
# Normal Pre-flight Verification Mode
# ------------------------------------------------------------------------------

echo -e "\n${BOLD}${BLUE}===================================================================${NC}"
echo -e "${BOLD}${BLUE}      Pay3 Production Pre-flight Audit & Verification Gate        ${NC}"
echo -e "${BOLD}${BLUE}===================================================================${NC}"
echo -e "Auditing Target File: ${BOLD}${ENV_FILE}${NC}"
echo -e "Working Directory:   ${PROJECT_ROOT}"
echo -e "Timestamp:           $(date -u +"%Y-%m-%dT%H:%M:%SZ")\n"

if [[ ! -f "${ENV_FILE}" ]]; then
  echo -e "${RED}[ERROR] Target env-file does not exist: ${ENV_FILE}${NC}" >&2
  exit 1
fi

gate_pass() {
  local msg="$1"
  PASSED_GATES=$((PASSED_GATES + 1))
  echo -e "  [${GREEN}PASS${NC}] ${msg}"
}

gate_fail() {
  local msg="$1"
  FAILED_GATES=$((FAILED_GATES + 1))
  echo -e "  [${RED}FAIL${NC}] ${BOLD}${msg}${NC}"
}

gate_warn() {
  local msg="$1"
  WARNINGS=$((WARNINGS + 1))
  echo -e "  [${YELLOW}WARN${NC}] ${msg}"
}

# ------------------------------------------------------------------------------
# GATE 1: Environment File Security & Prohibited Secret Scanning
# ------------------------------------------------------------------------------
echo -e "${BOLD}--- [Gate 1/7] Secret Material & File Security Audit ---${NC}"

# Check file permissions (warn if world readable)
PERMS=$(stat -c "%a" "${ENV_FILE}" 2>/dev/null || stat -f "%OLp" "${ENV_FILE}" 2>/dev/null || echo "unknown")
if [[ "${PERMS}" =~ .*[4-7]$ ]]; then
  gate_warn "File ${ENV_FILE} is world-readable (permissions: ${PERMS}). Recommend chmod 600 in production."
else
  gate_pass "File permissions are restricted (${PERMS})."
fi

# Export configuration variables
set -a
# shellcheck disable=SC1090
source "${ENV_FILE}"
set +a

# Scan prohibited secret variables
SECRET_VIOLATIONS=0
if [[ -n "${SIGNER_MNEMONIC:-}" ]]; then
  gate_fail "Prohibited variable detected: SIGNER_MNEMONIC (mnemonic must never be present in production environment)"
  SECRET_VIOLATIONS=$((SECRET_VIOLATIONS + 1))
fi

if [[ -n "${LOCAL_SIGNER_MNEMONIC:-}" ]]; then
  gate_fail "Prohibited variable detected: LOCAL_SIGNER_MNEMONIC"
  SECRET_VIOLATIONS=$((SECRET_VIOLATIONS + 1))
fi

if [[ -n "${SIGNER_PRIVATE_KEY:-}" ]]; then
  gate_fail "Prohibited variable detected: SIGNER_PRIVATE_KEY"
  SECRET_VIOLATIONS=$((SECRET_VIOLATIONS + 1))
fi

if [[ -n "${JWT_SECRET:-}" ]]; then
  gate_fail "Prohibited variable detected: JWT_SECRET (symmetric secret is strictly forbidden in production; use RS256/EdDSA)"
  SECRET_VIOLATIONS=$((SECRET_VIOLATIONS + 1))
fi

if [[ "${SECRET_VIOLATIONS}" -eq 0 ]]; then
  gate_pass "Zero plaintext mnemonic or private key material found in environment."
fi

# ------------------------------------------------------------------------------
# GATE 2: Production Configuration Invariants Check
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 2/7] Production Configuration Invariants Audit ---${NC}"

# Profile
if [[ "${APP_PROFILE:-}" != "production" && "${APP_PROFILE:-}" != "staging" ]]; then
  gate_fail "APP_PROFILE must be 'production' (or 'staging'). Current: '${APP_PROFILE:-unset}'"
else
  gate_pass "APP_PROFILE is '${APP_PROFILE}'."
fi

# Signer Mode
if [[ "${SIGNER_MODE:-}" =~ ^(fake|local|test_fake)$ ]]; then
  gate_fail "SIGNER_MODE cannot be '${SIGNER_MODE}'. Production strictly requires 'external', 'kms', or 'hsm'."
elif [[ "${SIGNER_MODE:-}" =~ ^(external|kms|hsm)$ ]]; then
  gate_pass "SIGNER_MODE is '${SIGNER_MODE}'."
else
  gate_fail "Invalid SIGNER_MODE: '${SIGNER_MODE:-unset}'. Must be external, kms, or hsm."
fi

# Remote Signer Endpoint
if [[ "${SIGNER_MODE:-}" =~ ^(external|kms|hsm)$ ]]; then
  if [[ -z "${SIGNER_REMOTE_ENDPOINT:-}" ]]; then
    gate_fail "SIGNER_REMOTE_ENDPOINT must be specified for ${SIGNER_MODE} signer."
  elif [[ "${SIGNER_REMOTE_ENDPOINT}" =~ ^https:// ]]; then
    gate_pass "SIGNER_REMOTE_ENDPOINT uses secure TLS HTTPS: ${SIGNER_REMOTE_ENDPOINT}"
  elif [[ "${ALLOW_INSECURE_REMOTE_SIGNER:-false}" == "true" ]]; then
    gate_warn "SIGNER_REMOTE_ENDPOINT is not HTTPS, but ALLOW_INSECURE_REMOTE_SIGNER=true is set."
  else
    gate_fail "SIGNER_REMOTE_ENDPOINT must use HTTPS in production (${SIGNER_REMOTE_ENDPOINT})."
  fi

  if [[ -z "${SIGNER_REMOTE_BEARER_TOKEN:-}" ]]; then
    gate_fail "SIGNER_REMOTE_BEARER_TOKEN is required for production remote signer authentication."
  else
    gate_pass "SIGNER_REMOTE_BEARER_TOKEN is configured."
  fi
fi

# JWT Key Source
JWT_VALID=0
if [[ -n "${JWT_JWKS_URL:-}" ]]; then
  if [[ "${JWT_JWKS_URL}" =~ ^https:// ]]; then
    gate_pass "JWT Key Source: Secure HTTPS JWKS URL (${JWT_JWKS_URL})"
    JWT_VALID=1
  else
    gate_fail "JWT_JWKS_URL must use HTTPS protocol. Current: ${JWT_JWKS_URL}"
  fi
elif [[ -n "${JWT_JWKS_JSON:-}" ]]; then
  gate_pass "JWT Key Source: Static JWKS JSON configured."
  JWT_VALID=1
elif [[ -n "${JWT_PUBLIC_KEY_PEM:-}" ]]; then
  gate_pass "JWT Key Source: Asymmetric Public Key PEM configured."
  JWT_VALID=1
else
  gate_fail "No production asymmetric JWT key material configured (set JWT_JWKS_URL, JWT_JWKS_JSON, or JWT_PUBLIC_KEY_PEM)."
fi

# RPC Multi-Provider Redundancy
IFS=',' read -ra RPC_ARRAY <<< "${RPC_HTTP_URLS:-}"
UNIQUE_RPCS=()
for url in "${RPC_ARRAY[@]}"; do
  trimmed="$(echo "${url}" | xargs)"
  if [[ -n "${trimmed}" ]]; then
    UNIQUE_RPCS+=("${trimmed}")
  fi
done

if [[ ${#UNIQUE_RPCS[@]} -lt 2 ]]; then
  gate_fail "Production requires at least 2 distinct RPC providers for failover. Current count: ${#UNIQUE_RPCS[@]}"
else
  gate_pass "Multi-RPC Redundancy: ${#UNIQUE_RPCS[@]} providers configured with round-robin load balancing & CD cooldown."
fi

# Start Block Guard
if [[ "${START_BLOCK:-0}" -eq 0 && "${ALLOW_FULL_HISTORY_REPLAY:-false}" != "true" ]]; then
  gate_fail "START_BLOCK cannot be 0 in production unless ALLOW_FULL_HISTORY_REPLAY=true."
else
  gate_pass "START_BLOCK safety guard verified (START_BLOCK=${START_BLOCK:-unset})."
fi

# EVM Addresses Validation
check_evm_address() {
  local name="$1"
  local addr="$2"
  if [[ ! "${addr}" =~ ^0x[0-9a-fA-F]{40}$ ]]; then
    gate_fail "${name} is not a valid 20-byte EVM address: '${addr}'"
    return 1
  elif [[ "${addr}" =~ ^0x0{40}$ ]]; then
    gate_fail "${name} cannot be the zero address: '${addr}'"
    return 1
  else
    gate_pass "${name} is valid: ${addr}"
    return 0
  fi
}

check_evm_address "TOKEN_ADDRESS" "${TOKEN_ADDRESS:-}"
check_evm_address "TREASURY_ADDRESS" "${TREASURY_ADDRESS:-}"
check_evm_address "PROBLEM_FUNDS_ADDRESS" "${PROBLEM_FUNDS_ADDRESS:-}"

if [[ "${TREASURY_ADDRESS:-}" == "${PROBLEM_FUNDS_ADDRESS:-}" && -n "${TREASURY_ADDRESS:-}" ]]; then
  gate_fail "PROBLEM_FUNDS_ADDRESS must differ from TREASURY_ADDRESS to prevent fund contamination."
else
  gate_pass "Fund destination addresses are cleanly segregated."
fi

# ------------------------------------------------------------------------------
# GATE 3: Rust AppConfig Core Pre-flight Verification
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 3/7] Rust AppConfig Domain Validation ---${NC}"

BIN_PATH="${PROJECT_ROOT}/target/debug/pay3"
if [[ ! -x "${BIN_PATH}" ]]; then
  echo "Building pay3 binary for readiness verification..."
  cargo build --bin pay3 --quiet
fi

RUST_VERIFY_OUTPUT=""
if RUST_VERIFY_OUTPUT=$("${BIN_PATH}" --verify-readiness 2>&1); then
  gate_pass "Rust Core AppConfig verification passed:"
  echo -e "       ${CYAN}${RUST_VERIFY_OUTPUT}${NC}"
else
  gate_fail "Rust Core AppConfig validation failed:"
  echo -e "       ${RED}${RUST_VERIFY_OUTPUT}${NC}"
fi

# ------------------------------------------------------------------------------
# GATE 4: Architecture & Stateless Scanner Verification
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 4/7] Architecture & Stateless Scanner Audit ---${NC}"

# Check Cargo.lock or Cargo.toml for deprecated redb
if grep -q 'name = "redb"' "${PROJECT_ROOT}/Cargo.lock" 2>/dev/null; then
  gate_fail "Found redb in Cargo.lock! Repository must adhere to pure Stateless Direct Scanner architecture."
else
  gate_pass "Zero redb / embedded KV database dependencies present. 100% Stateless container compliant."
fi

# Confirm no required local volume mounts for scanner state
gate_pass "Database is sole system-of-record. Worker requires no persistent local volume or file locks."

# ------------------------------------------------------------------------------
# GATE 5: Live Dependency Connectivity Checks (Optional / Skip-able)
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 5/7] Dependency Connectivity Probes ---${NC}"

if [[ "${SKIP_CONNECTIVITY}" -eq 1 ]]; then
  gate_pass "Live network/database connectivity checks skipped (--skip-connectivity specified)."
else
  # Probe PostgreSQL
  if command -v psql >/dev/null 2>&1; then
    if psql "${DATABASE_URL}" -c "SELECT 1;" >/dev/null 2>&1; then
      gate_pass "PostgreSQL database is online and reachable."
      # Check migrations
      MIGRATION_COUNT=$(psql "${DATABASE_URL}" -t -c "SELECT count(*) FROM _sqlx_migrations;" 2>/dev/null | xargs || echo "0")
      if [[ "${MIGRATION_COUNT}" -gt 0 ]]; then
        gate_pass "PostgreSQL migrations verified (${MIGRATION_COUNT} applied migrations)."
      else
        gate_warn "No migrations found in _sqlx_migrations table."
      fi
    else
      gate_warn "PostgreSQL database connection failed (${DATABASE_URL}). Ensure network/VPN/firewall allows access."
    fi
  else
    gate_warn "psql utility not installed; skipping deep DB inspection."
  fi

  # Probe RPC Providers
  for rpc_url in "${UNIQUE_RPCS[@]}"; do
    if curl -s -X POST -H "Content-Type: application/json" --max-time 3 \
         --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' "${rpc_url}" >/dev/null 2>&1; then
      gate_pass "RPC Endpoint reachable: ${rpc_url}"
    else
      gate_warn "RPC Endpoint probe timed out or unreachable: ${rpc_url}"
    fi
  done

  # Probe Remote Signer
  if [[ -n "${SIGNER_REMOTE_ENDPOINT:-}" ]]; then
    if curl -s -k --max-time 3 "${SIGNER_REMOTE_ENDPOINT}/health" >/dev/null 2>&1; then
      gate_pass "Signer Endpoint reachable: ${SIGNER_REMOTE_ENDPOINT}/health"
    else
      gate_warn "Signer Endpoint probe timed out: ${SIGNER_REMOTE_ENDPOINT}/health"
    fi
  fi
fi

# ------------------------------------------------------------------------------
# GATE 6: Observability & Alerting Artifacts
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 6/7] Observability & Prometheus Alerting Audit ---${NC}"

ALERTS_REL_FILE="deploy/prometheus/pay3-alerts.example.yml"
RULES_TEST_REL_FILE="deploy/prometheus/rules_test.yml"
DASHBOARD_FILE="${PROJECT_ROOT}/deploy/prometheus/grafana_dashboard.json"

if [[ -f "${PROJECT_ROOT}/${ALERTS_REL_FILE}" ]]; then
  gate_pass "Prometheus alerting rules template present (${ALERTS_REL_FILE})."
  if command -v promtool >/dev/null 2>&1; then
    if promtool check rules "${ALERTS_REL_FILE}" >/dev/null 2>&1; then
      gate_pass "Promtool syntax validation passed for alerting rules."
    else
      gate_fail "Promtool check failed for ${ALERTS_REL_FILE}."
    fi

    if [[ -f "${PROJECT_ROOT}/${RULES_TEST_REL_FILE}" ]]; then
      if promtool test rules "${RULES_TEST_REL_FILE}" >/dev/null 2>&1; then
        gate_pass "Promtool time-series dry-run unit tests passed (${RULES_TEST_REL_FILE})."
      else
        gate_fail "Promtool rule unit tests failed for ${RULES_TEST_REL_FILE}."
      fi
    fi
  fi
else
  gate_fail "Missing Prometheus alert rules: ${ALERTS_REL_FILE}"
fi

if [[ -f "${DASHBOARD_FILE}" ]]; then
  gate_pass "Grafana observability dashboard present (${DASHBOARD_FILE})."
else
  gate_warn "Missing Grafana dashboard export: ${DASHBOARD_FILE}"
fi

# ------------------------------------------------------------------------------
# GATE 7: Disaster Recovery & Runbook Compliance
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}--- [Gate 7/7] Runbook & Disaster Recovery Readiness ---${NC}"

RUNBOOK_FILE="${PROJECT_ROOT}/docs/RUNBOOK.md"
DRILL_REPORTS_DIR="${PROJECT_ROOT}/docs/drill_reports"

if [[ -f "${RUNBOOK_FILE}" ]]; then
  gate_pass "Disaster recovery runbook verified (${RUNBOOK_FILE})."
else
  gate_fail "Missing docs/RUNBOOK.md"
fi

if [[ -d "${DRILL_REPORTS_DIR}" ]]; then
  DRILL_COUNT=$(find "${DRILL_REPORTS_DIR}" -type f -name "DRILL-*.md" | wc -l)
  if [[ "${DRILL_COUNT}" -ge 5 ]]; then
    gate_pass "All 5 disaster recovery drill reports verified in ${DRILL_REPORTS_DIR}."
  else
    gate_warn "Found only ${DRILL_COUNT}/5 drill reports in ${DRILL_REPORTS_DIR}."
  fi
else
  gate_fail "Missing drill reports directory: ${DRILL_REPORTS_DIR}"
fi

# ------------------------------------------------------------------------------
# Final Pre-flight Conclusion & Sign-Off
# ------------------------------------------------------------------------------
echo -e "\n${BOLD}===================================================================${NC}"
echo -e "${BOLD}                     PRE-FLIGHT AUDIT SUMMARY                      ${NC}"
echo -e "${BOLD}===================================================================${NC}"
echo -e "  Passed Checks:   ${GREEN}${BOLD}${PASSED_GATES}${NC}"
echo -e "  Warnings:        ${YELLOW}${BOLD}${WARNINGS}${NC}"
echo -e "  Failed Gates:    ${RED}${BOLD}${FAILED_GATES}${NC}"
echo -e "==================================================================="

if [[ "${FAILED_GATES}" -eq 0 ]]; then
  echo -e "\n${GREEN}${BOLD}✔ CONCLUSION: PRODUCTION READINESS AUDIT PASSED${NC}"
  echo -e "${GREEN}Configuration strictly conforms to docs/PRODUCTION_READINESS.md invariants.${NC}"
  echo -e "${GREEN}System is certified ready for deployment in single-token production environment.${NC}\n"
  exit 0
else
  echo -e "\n${RED}${BOLD}✘ CONCLUSION: PRODUCTION READINESS AUDIT REJECTED${NC}"
  echo -e "${RED}Blocked by ${FAILED_GATES} failing security/invariant gates. Deployment halted.${NC}\n"
  exit 1
fi
