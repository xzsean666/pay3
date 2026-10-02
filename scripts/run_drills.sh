#!/usr/bin/env bash
set -euo pipefail

echo "=========================================================="
echo "          Pay3 Disaster Recovery Drill Suite              "
echo "=========================================================="

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${PROJECT_ROOT}"

echo ""
echo "[Drill 1/5] Verifying PostgreSQL Schema & Constraints (DRILL-001)..."
cargo test --test migration_contract
echo "==> DRILL-001 PASSED"

echo ""
echo "[Drill 2/5] Verifying Stateless Scanner & Reorg Rewind (DRILL-002)..."
cargo test --test reorg_integration
echo "==> DRILL-002 PASSED"

echo ""
echo "[Drill 3/5] Verifying Multi-RPC Load Balancing & Cooldown (DRILL-003)..."
cargo test chain::rpc
echo "==> DRILL-003 PASSED"

echo ""
echo "[Drill 4/5] Verifying Remote Signer Outage & Auth Hardening (DRILL-004)..."
cargo test --test signer_contract
echo "==> DRILL-004 PASSED"

echo ""
echo "[Drill 5/5] Verifying Collector Crash Recovery & Nonce Idempotency (DRILL-005)..."
cargo test --test collector_recovery_integration
echo "==> DRILL-005 PASSED"

echo ""
echo "[Prometheus Alerts] Checking alert rules & dry-run tests..."
if command -v promtool >/dev/null 2>&1; then
    promtool check rules deploy/prometheus/pay3-alerts.example.yml
    promtool test rules deploy/prometheus/rules_test.yml
    echo "==> Alert rules dry-run PASSED"
else
    echo "==> promtool not found in PATH, skipping promtool check"
fi

echo ""
echo "=========================================================="
echo "    ALL DISASTER RECOVERY DRILLS COMPLETED SUCCESSFULLY   "
echo "=========================================================="
