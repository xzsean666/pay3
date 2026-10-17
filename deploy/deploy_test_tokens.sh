#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Pay3 免 Gas 测试代币 (USDC / USDT) 部署脚本
# Deploy gasless test tokens (MockUSDC: EIP-3009, MockUSDT: Polygon MetaTx) on Polygon Amoy
# ==============================================================================

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
TOKENS_DIR="${SCRIPT_DIR}/tokens"

# 默认配置
KEY_FILE="${REPO_ROOT}/.env.key"
TEST_ENV_FILE="${REPO_ROOT}/.env.test"
OUTPUT_ENV_FILE="${SCRIPT_DIR}/tokens_amoy.env"
OUTPUT_JSON_FILE="${SCRIPT_DIR}/tokens_amoy.json"

RPC_URL="${RPC_URL:-https://polygon-amoy-bor-rpc.publicnode.com}"
CHAIN_ID=80002

if [[ -f "$KEY_FILE" ]]; then
  PRIVATE_KEY="${PRIVATE_KEY:-$(grep -E '^PRIVATE_KEY=' "$KEY_FILE" | cut -d= -f2- | tr -d ' "\r\n')}"
fi

if [[ -z "${PRIVATE_KEY:-}" ]]; then
  echo "Error: PRIVATE_KEY not found in environment or $KEY_FILE" >&2
  exit 1
fi

DEPLOYER_ADDRESS="$(cast wallet address --private-key "$PRIVATE_KEY")"
echo "Deployer address: $DEPLOYER_ADDRESS"

BALANCE_WEI="$(cast balance "$DEPLOYER_ADDRESS" --rpc-url "$RPC_URL")"
echo "Deployer balance: $BALANCE_WEI wei on Amoy ($RPC_URL)"

if [[ "$BALANCE_WEI" == "0" ]]; then
  echo "Error: Deployer balance is 0 on Amoy!" >&2
  exit 1
fi

cd "$TOKENS_DIR"
echo "Compiling contracts..."
forge build

INITIAL_SUPPLY="10000000000000" # 10,000,000 * 10^6

echo "------------------------------------------------------------"
echo "1. Deploying MockUSDC (EIP-3009 transferWithAuthorization)..."
echo "------------------------------------------------------------"
USDC_DEPLOY_OUT="$(forge create src/MockUSDC.sol:MockUSDC \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY" \
  --broadcast \
  --json \
  --constructor-args "$DEPLOYER_ADDRESS" "$INITIAL_SUPPLY")"

USDC_ADDRESS="$(echo "$USDC_DEPLOY_OUT" | jq -r '.deployedTo')"
if [[ -z "$USDC_ADDRESS" || "$USDC_ADDRESS" == "null" ]]; then
  echo "Failed to parse USDC deployed address from output: $USDC_DEPLOY_OUT" >&2
  exit 1
fi
echo "MockUSDC deployed at: $USDC_ADDRESS"

echo "------------------------------------------------------------"
echo "2. Deploying MockUSDT (Polygon NativeMetaTransaction)..."
echo "------------------------------------------------------------"
USDT_DEPLOY_OUT="$(forge create src/MockUSDT.sol:MockUSDT \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY" \
  --broadcast \
  --json \
  --constructor-args "$DEPLOYER_ADDRESS" "$INITIAL_SUPPLY")"

USDT_ADDRESS="$(echo "$USDT_DEPLOY_OUT" | jq -r '.deployedTo')"
if [[ -z "$USDT_ADDRESS" || "$USDT_ADDRESS" == "null" ]]; then
  echo "Failed to parse USDT deployed address from output: $USDT_DEPLOY_OUT" >&2
  exit 1
fi
echo "MockUSDT deployed at: $USDT_ADDRESS"

CURRENT_BLOCK="$(cast block-number --rpc-url "$RPC_URL")"

# 保存部署结果
cat <<EOF > "$OUTPUT_ENV_FILE"
# Deployed on Polygon Amoy (Chain ID 80002) at $(date -u +"%Y-%m-%dT%H:%M:%SZ")
AMOY_USDC_ADDRESS=$USDC_ADDRESS
AMOY_USDC_SYMBOL=USDC
AMOY_USDC_DECIMALS=6
AMOY_USDC_METHOD=eip3009

AMOY_USDT_ADDRESS=$USDT_ADDRESS
AMOY_USDT_SYMBOL=USDT
AMOY_USDT_DECIMALS=6
AMOY_USDT_METHOD=polygon_meta_tx

AMOY_START_BLOCK=$CURRENT_BLOCK
EOF

cat <<EOF > "$OUTPUT_JSON_FILE"
{
  "chainId": 80002,
  "deployedAt": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "startBlock": $CURRENT_BLOCK,
  "rpcUrl": "$RPC_URL",
  "deployer": "$DEPLOYER_ADDRESS",
  "tokens": {
    "USDC": {
      "address": "$USDC_ADDRESS",
      "symbol": "USDC",
      "decimals": 6,
      "collectionMethod": "eip3009",
      "name": "USD Coin",
      "version": "2"
    },
    "USDT": {
      "address": "$USDT_ADDRESS",
      "symbol": "USDT",
      "decimals": 6,
      "collectionMethod": "polygon_meta_tx",
      "name": "(PoS) Tether USD",
      "version": "1"
    }
  }
}
EOF

echo "------------------------------------------------------------"
echo "Deployment successful!"
echo "USDC: $USDC_ADDRESS"
echo "USDT: $USDT_ADDRESS"
echo "Start block: $CURRENT_BLOCK"
echo "Wrote results to $OUTPUT_ENV_FILE and $OUTPUT_JSON_FILE"
echo "------------------------------------------------------------"
