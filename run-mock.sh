#!/usr/bin/env bash
# Run OpenAirCare against the in-memory fake AirPods (no Bluetooth needed).
# MAKEPAD_GPU=gl avoids Vulkan quirks on older Intel GPUs / some Wayland setups.
set -euo pipefail
cd "$(dirname "$0")"
# shellcheck disable=SC1090
source "${CARGO_HOME:-$HOME/.cargo}/env" 2>/dev/null || true
export MAKEPAD_GPU="${MAKEPAD_GPU:-gl}"
exec cargo run -p openaircare --release --features mock "$@"
