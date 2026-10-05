#!/usr/bin/env bash
# Prepare a Linux computer for OpenAirCare and start the app.
# Installs libraries, sets the Apple Bluetooth vendor ID AirPods require,
# grants permission to open a Bluetooth socket, and pairs the earbuds.
if [ -z "${BASH_VERSION:-}" ]; then
  echo "Run this script with bash: ./setup.sh" >&2
  exit 1
fi
set -euo pipefail

export PATH="/usr/sbin:/sbin:$PATH"

CONF=/etc/bluetooth/main.conf
BAK=/etc/bluetooth/main.conf.openaircare-bak
MARK=/etc/bluetooth/main.conf.openaircare-was-missing
DEVICE_ID_VALUE="bluetooth:004C:0000:0000"

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
ASSUME_YES=0
NO_PAIR=0
NO_LAUNCH=0
SUDO_OK=0
ID_CHANGED=0
SETCAP_OK=0
BIN=""
# Set by --setcap / --add-user, which the app passes with --root-apply.
SETCAP_PATH=""
ADD_USER=""
PAIR_RESULT=skipped
FAILS=0

die() { printf '%s\n' "$*" >&2; exit 1; }
say() { printf '\n%s\n' "$*"; }
ok() { printf '  ok  %s\n' "$*"; }
note() { printf '  ..  %s\n' "$*"; }
warn() { printf '  !!  %s\n' "$*" >&2; }

usage() {
  cat <<EOF
Usage: ./setup.sh [options]

Prepare this computer for OpenAirCare and start the app.

The script installs libraries, tells Bluetooth to identify as an Apple
device (required for Hearing Health), and walks you through pairing.

Options:
  --yes         Apply the system changes without asking
  --no-pair     Skip AirPods pairing
  --no-launch   Do not start the app
  --undo        Restore Bluetooth to how it was before setup
  --help        Show this help
EOF
}

confirm() {
  local prompt=$1
  local default=${2:-y}
  local hint reply
  if [[ "$ASSUME_YES" == 1 ]]; then
    return 0
  fi
  if [[ ! -t 0 ]]; then
    [[ "$default" == y ]]
    return
  fi
  if [[ "$default" == y ]]; then
    hint="[Y/n]"
  else
    hint="[y/N]"
  fi
  read -r -p "$prompt $hint " reply || true
  reply=${reply//[[:space:]]/}
  if [[ -z "$reply" ]]; then
    [[ "$default" == y ]]
    return
  fi
  [[ "$reply" =~ ^[Yy]([Ee][Ss])?$ ]]
}

# Run a command as root: directly when already root (the app starts this
# script through pkexec), with sudo otherwise.
as_root() {
  if [[ "$(id -u)" -eq 0 ]]; then
    "$@"
  else
    sudo "$@"
  fi
}

ensure_sudo() {
  if [[ "$(id -u)" -eq 0 ]]; then
    return
  fi
  if [[ "$SUDO_OK" == 1 ]]; then
    sudo -v
    return
  fi
  if ! sudo -n true 2>/dev/null; then
    say "Your password is required to change system settings."
  fi
  sudo -v || die "Administrator rights are required. The manual steps are in SETUP.md."
  SUDO_OK=1
}

# Read the active DeviceID under [General]. Empty if it is unset or commented out.
current_device_id() {
  local file=$1
  awk '
    /^\[/ { in_g = ($0 ~ /^\[General\][[:space:]]*$/) }
    in_g && /^[[:space:]]*DeviceID[[:space:]]*=/ {
      sub(/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*/, "")
      sub(/[[:space:]]*$/, "")
      print
      exit
    }
  ' "$file"
}

# Write a copy of $1 to $2 with DeviceID set for an Apple host.
# Leaves an already-correct file unchanged. Exits 2 if no DeviceID was written.
patch_main_conf() {
  local src=$1 dest=$2
  awk '
    function is_ours(line,    v) {
      v = line
      sub(/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*/, "", v)
      sub(/[[:space:]]*$/, "", v)
      return tolower(v) == "bluetooth:004c:0000:0000"
    }
    function emit_id() {
      print "# OpenAirCare: Apple vendor ID for Hearing Health. Undo with ./setup.sh --undo"
      print "DeviceID = bluetooth:004C:0000:0000"
      print ""
      wrote = 1
    }
    BEGIN { in_general = 0; saw_general = 0; wrote = 0 }
    /^\[/ {
      if (in_general && !wrote) emit_id()
      in_general = ($0 ~ /^\[General\][[:space:]]*$/)
      if (in_general) saw_general = 1
    }
    in_general && /^[[:space:]]*DeviceID[[:space:]]*=/ {
      if (!wrote && is_ours($0)) {
        print
        wrote = 1
      } else if (!wrote) {
        emit_id()
      }
      next
    }
    { print }
    END {
      if (!saw_general) {
        if (NR > 0) print ""
        print "[General]"
        emit_id()
      } else if (in_general && !wrote) {
        emit_id()
      }
      if (!wrote) exit 2
    }
  ' "$src" > "$dest"
}

desktop_quote() {
  local s=$1
  s=${s//%/%%}
  case "$s" in
    *[[:space:]\"\'\\\$\`]*)
      s=${s//\\/\\\\}
      s=${s//\"/\\\"}
      s=${s//\`/\\\`}
      s=${s//\$/\\\$}
      printf '"%s"' "$s"
      ;;
    *)
      printf '%s' "$s"
      ;;
  esac
}

expect_file() {
  local name=$1 got=$2 expected=$3
  if cmp -s "$got" "$expected"; then
    printf 'ok  %s\n' "$name"
  else
    printf 'FAIL %s\n' "$name" >&2
    diff -u "$expected" "$got" >&2 || true
    FAILS=$((FAILS + 1))
  fi
}

expect_eq() {
  local name=$1 got=$2 expected=$3
  if [[ "$got" == "$expected" ]]; then
    printf 'ok  %s\n' "$name"
  else
    printf 'FAIL %s\n' "$name" >&2
    printf '  expected: [%s]\n  got:      [%s]\n' "$expected" "$got" >&2
    FAILS=$((FAILS + 1))
  fi
}

self_test() {
  local tmp in out out2
  tmp=$(mktemp -d)
  FAILS=0

  cat > "$tmp/insert-in" <<'EOF'
[General]
#Name = BlueZ

[Policy]
AutoEnable=true
EOF
  cat > "$tmp/insert-exp" <<'EOF'
[General]
#Name = BlueZ

# OpenAirCare: Apple vendor ID for Hearing Health. Undo with ./setup.sh --undo
DeviceID = bluetooth:004C:0000:0000

[Policy]
AutoEnable=true
EOF
  patch_main_conf "$tmp/insert-in" "$tmp/insert-out"
  expect_file "insert DeviceID before the next section" "$tmp/insert-out" "$tmp/insert-exp"
  patch_main_conf "$tmp/insert-out" "$tmp/insert-out2"
  expect_file "insert is idempotent" "$tmp/insert-out2" "$tmp/insert-out"

  cat > "$tmp/last-in" <<'EOF'
[General]
Name=BlueZ
EOF
  cat > "$tmp/last-exp" <<'EOF'
[General]
Name=BlueZ
# OpenAirCare: Apple vendor ID for Hearing Health. Undo with ./setup.sh --undo
DeviceID = bluetooth:004C:0000:0000

EOF
  patch_main_conf "$tmp/last-in" "$tmp/last-out"
  expect_file "append DeviceID when General is the last section" "$tmp/last-out" "$tmp/last-exp"

  cat > "$tmp/replace-in" <<'EOF'
[General]
DeviceID = usb:05AC:0000:0000
#Keep
EOF
  cat > "$tmp/replace-exp" <<'EOF'
[General]
# OpenAirCare: Apple vendor ID for Hearing Health. Undo with ./setup.sh --undo
DeviceID = bluetooth:004C:0000:0000

#Keep
EOF
  patch_main_conf "$tmp/replace-in" "$tmp/replace-out"
  expect_file "replace a different DeviceID" "$tmp/replace-out" "$tmp/replace-exp"

  printf '%s\n' '[General]' 'DeviceID=bluetooth:004C:0000:0000' > "$tmp/same-in"
  patch_main_conf "$tmp/same-in" "$tmp/same-out"
  expect_file "leave an already-correct DeviceID untouched" "$tmp/same-out" "$tmp/same-in"

  printf '%s\n' '[General]' '#DeviceID = bluetooth:1234:5678:abcd' > "$tmp/comment-in"
  expect_eq "ignore a commented DeviceID" "$(current_device_id "$tmp/comment-in")" ""
  patch_main_conf "$tmp/comment-in" "$tmp/comment-out"
  expect_eq "commented example is not the active id" "$(current_device_id "$tmp/comment-out")" "$DEVICE_ID_VALUE"

  printf '%s\n' 'JustAComment=1' > "$tmp/nosection-in"
  patch_main_conf "$tmp/nosection-in" "$tmp/nosection-out"
  expect_eq "create a General section when missing" "$(current_device_id "$tmp/nosection-out")" "$DEVICE_ID_VALUE"

  cat > "$tmp/dup-in" <<'EOF'
[General]
DeviceID = bluetooth:004C:0000:0000
DeviceID = usb:0000:0000:0000
EOF
  patch_main_conf "$tmp/dup-in" "$tmp/dup-out"
  expect_eq "drop a second DeviceID line" "$(grep -c '^DeviceID' "$tmp/dup-out")" "1"

  expect_eq "quote a plain path" "$(desktop_quote "/home/gail/OpenAirCare/openaircare")" "/home/gail/OpenAirCare/openaircare"
  expect_eq "quote a path with spaces" "$(desktop_quote "/home/gail/My Apps/openaircare")" '"/home/gail/My Apps/openaircare"'
  expect_eq "escape percent in a desktop path" "$(desktop_quote "/tmp/100%/openaircare")" "/tmp/100%%/openaircare"

  rm -rf "$tmp"
  if [[ "$FAILS" -eq 0 ]]; then
    printf 'setup.sh self-test passed\n'
    return 0
  fi
  printf '%s self-test failure(s)\n' "$FAILS" >&2
  return 1
}

find_binary() {
  BIN=""
  if [[ -f "$ROOT/openaircare" ]]; then
    chmod +x "$ROOT/openaircare" 2>/dev/null || true
    BIN="$ROOT/openaircare"
  elif [[ -f "$ROOT/target/release/openaircare" ]]; then
    BIN="$ROOT/target/release/openaircare"
  fi
}

pick_asound_package() {
  if apt-cache show libasound2t64 >/dev/null 2>&1; then
    printf '%s\n' libasound2t64
  else
    printf '%s\n' libasound2
  fi
}

package_installed() {
  dpkg -s "$1" >/dev/null 2>&1
}

collect_runtime_packages() {
  ASOUND_PKG=$(pick_asound_package)
  RUNTIME_PKGS=(
    bluez
    libcap2-bin
    libdbus-1-3
    libpulse0
    libx11-6
    libxcursor1
    libxkbcommon0
    libxrandr2
    libxi6
    libwayland-client0
    libwayland-cursor0
    libwayland-egl1
    libegl1
    libgl1
    libgles2
    libgbm1
    libdrm2
    "$ASOUND_PKG"
  )
}

any_package_missing() {
  local pkg
  command -v apt-get >/dev/null || return 1
  collect_runtime_packages
  for pkg in "${RUNTIME_PKGS[@]}"; do
    package_installed "$pkg" || return 0
  done
  return 1
}

install_packages() {
  local -a missing=()
  local pkg
  if ! command -v apt-get >/dev/null; then
    if ! command -v bluetoothctl >/dev/null; then
      die "This script installs packages with apt (Debian, Ubuntu, Raspberry Pi OS). Install bluez, then run ./setup.sh again. Manual steps are in SETUP.md."
    fi
    note "This is not a Debian-based system. Skipping package install."
    return
  fi
  collect_runtime_packages
  for pkg in "${RUNTIME_PKGS[@]}"; do
    package_installed "$pkg" || missing+=("$pkg")
  done
  if [[ "${#missing[@]}" -eq 0 ]]; then
    ok "Libraries already installed."
    return
  fi
  say "Installing Bluetooth and graphics libraries."
  ensure_sudo
  as_root apt-get update
  if ! as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}"; then
    if [[ "$ASOUND_PKG" == libasound2t64 ]]; then
      missing=("${missing[@]/libasound2t64/libasound2}")
      as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}" \
        || die "Could not install packages. The list is in SETUP.md."
    else
      die "Could not install packages. The list is in SETUP.md."
    fi
  fi
  ok "Libraries installed."
}

read_conf_to() {
  local dest=$1
  if [[ -r "$CONF" ]]; then
    cp "$CONF" "$dest"
  elif [[ -e "$CONF" ]]; then
    ensure_sudo
    as_root cp "$CONF" "$dest"
    as_root chown "$USER" "$dest"
  else
    : > "$dest"
  fi
}

backup_original_once() {
  if [[ -e "$BAK" || -e "$MARK" ]]; then
    return
  fi
  ensure_sudo
  if [[ -e "$CONF" ]]; then
    as_root cp -a "$CONF" "$BAK"
  else
    as_root mkdir -p /etc/bluetooth
    as_root touch "$MARK"
  fi
}

configure_bluetooth() {
  local src patched old
  if ! command -v systemctl >/dev/null; then
    die "Bluetooth setup needs systemd. Manual steps are in SETUP.md."
  fi
  say "Configuring Bluetooth."
  src=$(mktemp)
  patched=$(mktemp)
  read_conf_to "$src"
  old=$(current_device_id "$src")
  if [[ -n "$old" && "${old,,}" != "${DEVICE_ID_VALUE,,}" ]]; then
    echo "Bluetooth currently identifies as: $old"
    echo "AirPods only open Hearing Health settings to an Apple identity ($DEVICE_ID_VALUE)."
    if ! confirm "Replace the current identity?"; then
      note "Leaving the Bluetooth identity unchanged. Hearing Health settings will stay unavailable."
      rm -f "$src" "$patched"
      return
    fi
  fi
  patch_main_conf "$src" "$patched" || {
    rm -f "$src" "$patched"
    die "Could not update $CONF."
  }
  if cmp -s "$src" "$patched"; then
    ok "Bluetooth already identifies as an Apple device."
    rm -f "$src" "$patched"
    return
  fi
  backup_original_once
  ensure_sudo
  if [[ ! -d /etc/bluetooth ]]; then
    as_root mkdir -p /etc/bluetooth
  fi
  as_root install -m 644 -o root -g root "$patched" "$CONF"
  rm -f "$src" "$patched"
  ID_CHANGED=1
  ok "Saved the Apple vendor ID to $CONF."
  echo "Restarting Bluetooth. Mice, keyboards, and headphones on Bluetooth will drop for a few seconds."
  if command -v rfkill >/dev/null; then
    as_root rfkill unblock bluetooth || true
  fi
  as_root systemctl enable bluetooth >/dev/null || true
  as_root systemctl restart bluetooth || die "Could not restart Bluetooth. Check: systemctl status bluetooth"
  wait_for_apple_identity
}

wait_for_apple_identity() {
  local i show
  for ((i = 1; i <= 30; i++)); do
    show=$(timeout 5 bluetoothctl show 2>&1 || true)
    case "$show" in
      *v004[Cc]*)
        ok "Bluetooth is identifying as Apple."
        bluetoothctl power on >/dev/null 2>&1 || true
        return
        ;;
      *"No default controller"*)
        warn "No Bluetooth adapter found. The Apple identity is saved and applies when an adapter is present."
        return
        ;;
    esac
    if ((i == 5)); then
      bluetoothctl power on >/dev/null 2>&1 || true
    fi
    sleep 0.5
  done
  warn "Bluetooth restarted, but the adapter is not showing the Apple identity yet."
  echo "  Check with: bluetoothctl show | grep Modalias"
  echo "  The Modalias line should contain v004C. You can run ./setup.sh again."
}

caps_present() {
  local caps
  caps=$(getcap "$1" 2>/dev/null || true)
  case "$caps" in
    *cap_net_raw*cap_net_admin*|*cap_net_admin*cap_net_raw*) return 0 ;;
    *) return 1 ;;
  esac
}

grant_permissions() {
  local groups
  if [[ -n "$BIN" ]]; then
    if caps_present "$BIN"; then
      SETCAP_OK=1
      ok "This copy of the app can use Bluetooth."
    else
      say "Allowing OpenAirCare to open Bluetooth connections."
      ensure_sudo
      if as_root setcap cap_net_raw,cap_net_admin+eip "$BIN" && caps_present "$BIN"; then
        SETCAP_OK=1
        ok "This copy of the app can use Bluetooth."
      else
        warn "Could not grant Bluetooth access on the app file."
        echo "  Move this folder into your home directory (not a USB stick or a protected folder) and run ./setup.sh again."
      fi
    fi
  fi
  groups=$(id -nG "$USER" || true)
  case " $groups " in
    *" bluetooth "*)
      ok "Your user is in the bluetooth group."
      ;;
    *)
      if getent group bluetooth >/dev/null; then
        ensure_sudo
        as_root usermod -aG bluetooth "$USER"
        ok "Added $USER to the bluetooth group. That applies the next time you log in."
      else
        note "There is no bluetooth group on this system. The app permission above covers it."
      fi
      ;;
  esac
}

install_desktop_entry() {
  local apps file bin_q
  [[ -n "$BIN" ]] || return 0
  apps=${XDG_DATA_HOME:-$HOME/.local/share}/applications
  file=$apps/openaircare.desktop
  bin_q=$(desktop_quote "$BIN")
  mkdir -p "$apps"
  cat > "$file" <<EOF
[Desktop Entry]
Type=Application
Name=OpenAirCare
Comment=AirPods Pro hearing settings
Exec=env MAKEPAD_GPU=gl $bin_q
Path=$(dirname "$BIN")
Icon=audio-headphones
Terminal=false
Categories=Audio;Settings;
Keywords=AirPods;Bluetooth;Hearing;
StartupNotify=true
EOF
  ok "App menu entry is ready. Look for OpenAirCare."
}

valid_mac() {
  [[ "$1" =~ ^([0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$ ]]
}

# Fill MACS and NAMES from `bluetoothctl devices` output on stdin.
parse_device_lines() {
  local line mac name
  MACS=()
  NAMES=()
  while IFS= read -r line; do
    [[ "$line" =~ ^Device[[:space:]]+([0-9A-Fa-f:]{17})[[:space:]]+(.*)$ ]] || continue
    mac=${BASH_REMATCH[1]^^}
    name=${BASH_REMATCH[2]}
    valid_mac "$mac" || continue
    MACS+=("$mac")
    NAMES+=("$name")
  done
}

load_devices() {
  local lines
  lines=$(bluetoothctl devices 2>/dev/null || true)
  parse_device_lines <<< "$lines"
}

load_paired_airpods() {
  local lines info i
  PAIRED_MAC=()
  PAIRED_NAME=()
  lines=$(bluetoothctl devices Paired 2>/dev/null || true)
  if [[ -z "$lines" ]]; then
    lines=$(bluetoothctl devices 2>/dev/null || true)
  fi
  parse_device_lines <<< "$lines"
  [[ "${#MACS[@]}" -eq 0 ]] && return
  for i in "${!MACS[@]}"; do
    case "${NAMES[$i],,}" in
      *airpods*) ;;
      *) continue ;;
    esac
    info=$(bluetoothctl info "${MACS[$i]}" 2>/dev/null || true)
    case "$info" in
      *"Paired: yes"*)
        PAIRED_MAC+=("${MACS[$i]}")
        PAIRED_NAME+=("${NAMES[$i]}")
        ;;
    esac
  done
}

scan_for_candidates() {
  local i mac known
  CAND_MAC=()
  CAND_NAME=()
  note "Scanning for about 20 seconds. Leave the lid open."
  load_devices
  known=" "
  for mac in "${MACS[@]+"${MACS[@]}"}"; do
    known+="$mac "
  done
  # bluetoothd records what it finds; the client output is not needed.
  timeout 25 bluetoothctl --timeout 18 scan on >/dev/null 2>&1 || true
  load_devices
  if [[ "${#MACS[@]}" -gt 0 ]]; then
    for i in "${!MACS[@]}"; do
      case "${NAMES[$i],,}" in
        *airpods*)
          CAND_MAC+=("${MACS[$i]}")
          CAND_NAME+=("${NAMES[$i]}")
          ;;
      esac
    done
  fi
  if [[ "${#CAND_MAC[@]}" -gt 0 || "${#MACS[@]}" -eq 0 ]]; then
    return
  fi
  for i in "${!MACS[@]}"; do
    case "$known" in
      *" ${MACS[$i]} "*) continue ;;
    esac
    CAND_MAC+=("${MACS[$i]}")
    CAND_NAME+=("${NAMES[$i]}")
  done
}

pick_candidate() {
  local i choice count
  PICKED=""
  count=${#CAND_MAC[@]}
  if [[ "$count" -eq 0 ]]; then
    return 1
  fi
  if [[ "$count" -eq 1 ]]; then
    echo "Found ${CAND_NAME[0]} (${CAND_MAC[0]})."
    if confirm "Pair this device?"; then
      PICKED=${CAND_MAC[0]}
    fi
    return 0
  fi
  echo "Nearby devices:"
  for i in "${!CAND_MAC[@]}"; do
    printf '  %d) %s (%s)\n' "$((i + 1))" "${CAND_NAME[$i]}" "${CAND_MAC[$i]}"
  done
  while true; do
    read -r -p "Choose a number, or press Enter to cancel: " choice || return 0
    choice=${choice//[[:space:]]/}
    [[ -z "$choice" ]] && return 0
    if [[ "$choice" =~ ^[0-9]+$ ]] && ((choice >= 1 && choice <= count)); then
      PICKED=${CAND_MAC[$((choice - 1))]}
      return 0
    fi
    echo "Enter a number from 1 to $count."
  done
}

pair_mac() {
  local mac=$1
  note "Pairing $mac"
  timeout 90 bluetoothctl <<EOF || true
power on
agent NoInputNoOutput
default-agent
pair $mac
trust $mac
connect $mac
quit
EOF
  local info
  info=$(bluetoothctl info "$mac" 2>/dev/null || true)
  case "$info" in
    *"Paired: yes"*)
      bluetoothctl trust "$mac" >/dev/null 2>&1 || true
      case "$info" in
        *"Connected: yes"*)
          ok "Paired and connected."
          ;;
        *)
          timeout 20 bluetoothctl connect "$mac" >/dev/null 2>&1 || true
          ok "Paired. Take the AirPods out of the case if they are not connected yet."
          ;;
      esac
      return 0
      ;;
  esac
  return 1
}

# Returns 1 when the user keeps the current pairing, so the caller does not scan.
remove_paired_airpods() {
  local prompt=$1
  local i
  load_paired_airpods
  if [[ "${#PAIRED_MAC[@]}" -eq 0 ]]; then
    return 0
  fi
  echo "$prompt"
  for i in "${!PAIRED_MAC[@]}"; do
    echo "  ${PAIRED_NAME[$i]} (${PAIRED_MAC[$i]})"
  done
  if ! confirm "Remove them and pair again?"; then
    note "Leaving the current pairing in place. Hearing Health may stay unavailable until they are paired again."
    return 1
  fi
  for i in "${!PAIRED_MAC[@]}"; do
    bluetoothctl remove "${PAIRED_MAC[$i]}" >/dev/null 2>&1 || true
    ok "Removed ${PAIRED_NAME[$i]}."
  done
}

pair_airpods() {
  local attempt
  PAIR_RESULT=skipped
  if [[ "$NO_PAIR" == 1 ]]; then
    return
  fi
  if [[ ! -t 0 ]]; then
    note "Skipping pairing (there is no terminal). Pair the AirPods in Bluetooth settings, then open OpenAirCare."
    return
  fi
  if ! command -v bluetoothctl >/dev/null; then
    warn "bluetoothctl is not available, so pairing was skipped."
    return
  fi
  if ! command -v timeout >/dev/null; then
    note "Pair the AirPods in the system Bluetooth settings, then open OpenAirCare."
    return
  fi
  bluetoothctl power on >/dev/null 2>&1 || true
  say "Pairing AirPods."
  if [[ "$ID_CHANGED" == 1 ]]; then
    echo "AirPods remember the previous computer identity. They need to be paired again."
    if ! remove_paired_airpods "These AirPods were paired before this computer identified as Apple:"; then
      PAIR_RESULT=skipped
      return
    fi
  else
    load_paired_airpods
    if [[ "${#PAIRED_MAC[@]}" -gt 0 ]]; then
      echo "Already paired: ${PAIRED_NAME[0]} (${PAIRED_MAC[0]})."
      timeout 20 bluetoothctl connect "${PAIRED_MAC[0]}" >/dev/null 2>&1 \
        || note "Take the AirPods out of the case so they connect."
      PAIR_RESULT=already
      if ! confirm "Pair again anyway?" n; then
        return
      fi
      if ! remove_paired_airpods "Remove the current pairing, then pair them again:"; then
        return
      fi
    fi
  fi
  echo "1. Put the AirPods in the case and leave the lid open."
  echo "2. Hold the button on the back of the case until the light flashes white."
  read -r -p "Press Enter when the light is flashing white... " _ || true
  for ((attempt = 1; attempt <= 3; attempt++)); do
    scan_for_candidates
    if [[ "${#CAND_MAC[@]}" -eq 0 ]]; then
      warn "No AirPods showed up."
      if ((attempt < 3)) && confirm "Scan again?"; then
        continue
      fi
      PAIR_RESULT=failed
      echo "Pair them in the system Bluetooth settings (lid open, light flashing white), then open OpenAirCare."
      return
    fi
    pick_candidate
    if [[ -z "$PICKED" ]]; then
      note "Pairing cancelled."
      return
    fi
    if pair_mac "$PICKED"; then
      PAIR_RESULT=paired
      return
    fi
    warn "Pairing did not finish."
    if ((attempt < 3)) && confirm "Try again?"; then
      echo "Hold the case button until the light flashes white."
      read -r -p "Press Enter to scan again... " _ || true
      continue
    fi
    PAIR_RESULT=failed
    echo "Pair them in the system Bluetooth settings, then open OpenAirCare."
    return
  done
}

launch_app() {
  if [[ "$NO_LAUNCH" == 1 ]]; then
    return
  fi
  if [[ -n "$BIN" ]]; then
    echo "You can also open OpenAirCare from the app menu later."
    if confirm "Start OpenAirCare now?"; then
      export MAKEPAD_GPU="${MAKEPAD_GPU:-gl}"
      exec "$BIN"
    fi
    return
  fi
  if [[ -x "$ROOT/run.sh" ]]; then
    note "This folder has the source code, not a packaged app."
    if confirm "Build and start it now?"; then
      exec "$ROOT/run.sh"
    fi
    return
  fi
  note "Bluetooth is ready. Build the app with: cargo run -p openaircare --release"
}

changes_needed() {
  local old groups
  if any_package_missing || ! command -v bluetoothctl >/dev/null; then
    return 0
  fi
  if [[ -r "$CONF" ]]; then
    old=$(current_device_id "$CONF")
  elif [[ -e "$CONF" ]]; then
    return 0
  else
    old=""
  fi
  if [[ "${old,,}" != "${DEVICE_ID_VALUE,,}" ]]; then
    return 0
  fi
  if [[ -n "$BIN" ]] && ! caps_present "$BIN"; then
    return 0
  fi
  groups=$(id -nG "$USER" || true)
  case " $groups " in
    *" bluetooth "*) ;;
    *)
      if getent group bluetooth >/dev/null; then
        return 0
      fi
      ;;
  esac
  return 1
}

undo_setup() {
  say "Restoring Bluetooth to how it was before OpenAirCare setup."
  echo "Mice, keyboards, and headphones on Bluetooth will drop for a few seconds."
  confirm "Continue?" || exit 0
  if [[ ! -e "$BAK" && ! -e "$MARK" ]]; then
    echo "No saved Bluetooth config was found, so nothing was changed."
    echo "If the Apple identity was added by hand, remove this line from $CONF and restart Bluetooth:"
    echo "  DeviceID = $DEVICE_ID_VALUE"
    exit 1
  fi
  ensure_sudo
  if [[ -e "$MARK" ]]; then
    as_root rm -f "$CONF" "$MARK"
    ok "Removed the Bluetooth config that setup created."
  else
    as_root cp -a "$BAK" "$CONF"
    as_root rm -f "$BAK"
    ok "Restored $CONF."
  fi
  as_root systemctl restart bluetooth || die "Could not restart Bluetooth."
  ok "Bluetooth restarted."
  echo "Pair the AirPods again if they stop connecting. The app itself was left in place."
}

# The root-only steps with no questions, for the Setup tab in the app
# (pkexec bash setup.sh --root-apply [--setcap PATH] [--add-user NAME]).
root_apply() {
  [[ "$(id -u)" -eq 0 ]] || die "--root-apply has to run as root."
  ASSUME_YES=1
  command -v bluetoothctl >/dev/null \
    || die "Bluetooth (the bluez package) is not installed. Install it, then try again."
  configure_bluetooth
  if [[ -n "$SETCAP_PATH" ]]; then
    command -v setcap >/dev/null \
      || die "setcap is not installed (package libcap2-bin). Install it, then try again."
    setcap cap_net_raw,cap_net_admin+eip "$SETCAP_PATH" \
      || die "Could not grant Bluetooth access on the app file. Move the app into your home directory and try again."
    ok "This copy of the app can use Bluetooth."
  fi
  if [[ -n "$ADD_USER" ]] && getent group bluetooth >/dev/null; then
    usermod -aG bluetooth "$ADD_USER" \
      || warn "Could not add $ADD_USER to the bluetooth group."
  fi
}

main() {
  local do_undo=0 do_test=0 do_root=0 desktop_bin=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --yes) ASSUME_YES=1 ;;
      --no-pair) NO_PAIR=1 ;;
      --no-launch) NO_LAUNCH=1 ;;
      --undo) do_undo=1 ;;
      --self-test) do_test=1 ;;
      --root-apply) do_root=1 ;;
      --setcap) shift; SETCAP_PATH=${1:-} ;;
      --add-user) shift; ADD_USER=${1:-} ;;
      --desktop-entry) shift; desktop_bin=${1:-} ;;
      -h|--help) usage; exit 0 ;;
      *) die "Unknown option: $1 (try ./setup.sh --help)" ;;
    esac
    shift
  done
  if [[ "$do_test" == 1 ]]; then
    self_test
    exit
  fi
  if [[ "$do_undo" == 1 ]]; then
    undo_setup
    exit 0
  fi
  if [[ "$do_root" == 1 ]]; then
    root_apply
    exit 0
  fi
  if [[ -n "$desktop_bin" ]]; then
    BIN=$desktop_bin
    install_desktop_entry
    exit 0
  fi

  if [[ "$(id -u)" -eq 0 ]]; then
    die "Run ./setup.sh as your everyday user. It will ask for your password when it needs it."
  fi
  if [[ ! -t 0 && "$ASSUME_YES" != 1 ]]; then
    die "Run ./setup.sh in a terminal, or pass --yes."
  fi

  find_binary
  say "OpenAirCare setup"
  echo "OpenAirCare is not a medical device. The app shows a safety notice before it connects."

  if changes_needed; then
    echo
    echo "This installs anything missing, tells Bluetooth to identify as an Apple device"
    echo "(AirPods require that for Hearing Health), and pairs the earbuds."
    echo "If Bluetooth has to restart, mice, keyboards, and headphones drop for a few seconds."
    confirm "Continue?" || exit 0
  else
    ok "This computer is already set up for OpenAirCare."
  fi

  install_packages
  configure_bluetooth
  grant_permissions
  install_desktop_entry
  pair_airpods

  say "Setup complete."
  case "$PAIR_RESULT" in
    failed)
      warn "The AirPods are not paired yet."
      ;;
    paired|already)
      echo "Wear the AirPods (keep them out of the case)."
      ;;
    *)
      echo "Pair the AirPods in Bluetooth settings if you have not already, then wear them."
      ;;
  esac
  echo "In the app, open Status and press Connect."
  launch_app
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
