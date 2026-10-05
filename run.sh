#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"
source "${CARGO_HOME:-$HOME/.cargo}/env" 2>/dev/null || true
export MAKEPAD_GPU="${MAKEPAD_GPU:-gl}"
exec cargo run -p openaircare --release "$@"
