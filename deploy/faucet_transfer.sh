#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# Pay3 领水脚本 (Faucet Transfer Script)
# Transfer mock USDC / USDT to any specified address on Polygon Amoy
# ==============================================================================

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

KEY_FILE="${REPO_ROOT}/.env.key"
RPC_URL="${RPC_URL:-https://polygon-amoy-bor-rpc.publicnode.com}"

USDC_ADDRESS="${USDC_ADDRESS:-0xc252a473DC466de3799149A2616ccc625832F40C}"
USDT_ADDRESS="${USDT_ADDRESS:-0x2C2e569e3Df89c55Ba46Ce52248Ae180379d0B31}"

RECIPIENT="${1:-}"
AMOUNT="${2:-1000}" # 默认转账 1,000 tokens

if [[ -z "$RECIPIENT" ]]; then
  echo "Usage: $0 <recipient_address> [amount_in_tokens]"
  echo "Example: $0 0x1234567890123456789012345678901234567890 1000"
  exit 1
fi

if [[ -f "$KEY_FILE" ]]; then
  PRIVATE_KEY="${PRIVATE_KEY:-$(grep -E '^PRIVATE_KEY=' "$KEY_FILE" | cut -d= -f2- | tr -d ' "\r\n')}"
fi

if [[ -z "${PRIVATE_KEY:-}" ]]; then
  echo "Error: PRIVATE_KEY not found in $KEY_FILE or environment!" >&2
  exit 1
fi

SENDER_ADDR="$(cast wallet address --private-key "$PRIVATE_KEY")"
echo "============================================================"
echo "Pay3 Token Faucet on Polygon Amoy (80002)"
echo "Sender:    $SENDER_ADDR"
echo "Recipient: $RECIPIENT"
echo "Amount:    $AMOUNT tokens (6 decimals)"
echo "RPC:       $RPC_URL"
echo "============================================================"

# Convert to 6 decimals (1 token = 1000000 raw units)
RAW_AMOUNT=$(python3 -c "print(int(float('$AMOUNT') * 10**6))")

echo "1. Transferring $AMOUNT MockUSDC ($USDC_ADDRESS)..."
cast send "$USDC_ADDRESS" "transfer(address,uint256)(bool)" "$RECIPIENT" "$RAW_AMOUNT" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY" \
  --priority-gas-price 30gwei

echo "2. Transferring $AMOUNT MockUSDT ($USDT_ADDRESS)..."
cast send "$USDT_ADDRESS" "transfer(address,uint256)(bool)" "$RECIPIENT" "$RAW_AMOUNT" \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY" \
  --priority-gas-price 30gwei

echo "============================================================"
echo "Successfully sent $AMOUNT USDC and $AMOUNT USDT to $RECIPIENT!"
echo "Check balances:"
USDC_BAL=$(cast call "$USDC_ADDRESS" "balanceOf(address)(uint256)" "$RECIPIENT" --rpc-url "$RPC_URL" | awk '{print $1}')
USDT_BAL=$(cast call "$USDT_ADDRESS" "balanceOf(address)(uint256)" "$RECIPIENT" --rpc-url "$RPC_URL" | awk '{print $1}')
python3 -c "print(f'USDC Balance: {int(\"$USDC_BAL\") / 10**6:.2f} USDC'); print(f'USDT Balance: {int(\"$USDT_BAL\") / 10**6:.2f} USDT')"
echo "============================================================"
