#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/generate-mnemonic.sh [--words 12|15|18|21|24] [--accounts N] [--write-env] [--env-file PATH] [--show-private-keys]

Generates a BIP39 mnemonic for local/testnet SIGNER_MODE=local.

Options:
  --words N       Mnemonic word count. Default: 12.
  --accounts N    Number of derived accounts to display. Default: 1.
  --write-env     Replace or append SIGNER_MNEMONIC in the env file.
  --env-file PATH Env file to update when --write-env is set. Default: .env.
  --show-private-keys
                  Print derived private keys from cast output. Default: redacted.
  -h, --help      Show this help.

Requires Foundry cast: https://book.getfoundry.sh/cast/
USAGE
}

words=12
accounts=1
write_env=0
env_file=".env"
show_private_keys=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --words)
      words="${2:-}"
      shift 2
      ;;
    --accounts)
      accounts="${2:-}"
      shift 2
      ;;
    --write-env)
      write_env=1
      shift
      ;;
    --env-file)
      env_file="${2:-}"
      shift 2
      ;;
    --show-private-keys)
      show_private_keys=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "$words" in
  12|15|18|21|24) ;;
  *)
    echo "--words must be one of 12, 15, 18, 21, 24" >&2
    exit 2
    ;;
esac

if ! [[ "$accounts" =~ ^[1-9][0-9]*$ ]]; then
  echo "--accounts must be a positive integer" >&2
  exit 2
fi

if ! command -v cast >/dev/null 2>&1; then
  echo "cast is required. Install Foundry first: https://book.getfoundry.sh/getting-started/installation" >&2
  exit 127
fi

output="$(cast wallet new-mnemonic --words "$words" --accounts "$accounts")"
mnemonic="$(printf '%s\n' "$output" | awk '/^Phrase:/{getline; print; exit}')"

if [[ -z "$mnemonic" ]]; then
  echo "failed to parse mnemonic from cast output" >&2
  exit 1
fi

if [[ "$show_private_keys" -eq 1 ]]; then
  printf '%s\n' "$output"
else
  printf '%s\n' "$output" | sed -E 's#^(Private key:[[:space:]]+).*#\1<redacted>#'
fi

if [[ "$write_env" -eq 1 ]]; then
  if [[ -e "$env_file" ]]; then
    if grep -q '^SIGNER_MNEMONIC=' "$env_file"; then
      sed -i "s#^SIGNER_MNEMONIC=.*#SIGNER_MNEMONIC=\"$mnemonic\"#" "$env_file"
    else
      printf '\nSIGNER_MNEMONIC="%s"\n' "$mnemonic" >> "$env_file"
    fi
  else
    printf 'SIGNER_MNEMONIC="%s"\n' "$mnemonic" > "$env_file"
  fi
  echo "updated $env_file: SIGNER_MNEMONIC=<redacted>"
fi
