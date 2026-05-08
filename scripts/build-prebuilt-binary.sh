#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

binary_name="pay3"
output_path="${PAY3_PREBUILT_BINARY:-deploy/prebuilt/pay3}"
cargo_target_dir="${CARGO_TARGET_DIR:-target}"
cargo_args=(build --release --locked)

if [[ -n "${PAY3_CARGO_TARGET:-}" ]]; then
  cargo_args+=(--target "$PAY3_CARGO_TARGET")
  built_binary="$cargo_target_dir/$PAY3_CARGO_TARGET/release/$binary_name"
else
  built_binary="$cargo_target_dir/release/$binary_name"
fi

cargo "${cargo_args[@]}"

install -d -m 0755 "$(dirname "$output_path")"
install -m 0755 "$built_binary" "$output_path"

printf 'Built prebuilt Docker binary: %s\n' "$output_path"
