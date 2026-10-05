# OpenAirCare (Rust + Makepad, Linux)

A Linux BlueZ configuration utility for Apple AirPods Pro audio transparency
profiles.

> [!WARNING]
> **Medical and safety disclaimer.** This software is an uncertified,
> experimental research utility for Linux hardware interoperability. It is
> **NOT** a medical device, is **NOT** approved by any regulatory health agency, and must **NOT** be used as a
> replacement for a prescribed hearing aid. The developers accept no liability
> for hearing damage or device malfunction.
>
> If you have, or suspect, hearing loss, see a qualified hearing-care
> professional. Stop using the software and remove the AirPods immediately if
> you notice discomfort, ringing, or unexpectedly loud output.

OpenAirCare is a small standalone app for hardware-interoperability research:
it lets a Linux computer (x86_64 or ARM) read and write the transparency-mode
audio profile settings that AirPods Pro 2 / AirPods Pro 3 already expose over
Bluetooth. It adds no audio processing of its own; it only relays settings to
the earbuds' firmware.

To install a prebuilt app, download a [release](https://github.com/brettpreston/OpenAirCare/releases), unpack it, and open `openaircare`. Its **Setup** tab configures Bluetooth and pairs the AirPods; `./setup.sh` does the same from a terminal and also installs missing libraries. See [SETUP.md](SETUP.md). Building from source is under [Build & run](#build--run).

- toggle the earbuds' built-in transparency audio profile — shown in the app
  as **Hearing Health** (Apple labels this setting "Hearing Aid") — plus the
  listening-mode selector, because the profile only applies in Transparency
  mode,
- edit the 8-band-per-ear profile curve (dB HL values) or load it from JSON,
- run a pure-tone **Hearing Test** (Apple-style beeps, one ear at a time) that
  fills the audiogram for you to review (see below),
- adjust amplification, balance, tone, ambient noise reduction and
  conversation boost,
- "swipe to control amplification" toggle, reset.

## Hearing Test tab

The Hearing Test tab plays three short beeps at each audiogram band (250 Hz
to 8 kHz), left ear then right, and steps the level up and down (modified
Hughson-Westlake) until it finds the quietest level you respond to. Press
the big button or Space when you hear the beeps. The result fills the
Audiogram tab, marked as unsaved, so you can review it and press *Apply*.

It is a screening aid, not a clinical test:

- A Linux host has no calibrated path to the earbuds. The app converts dB HL
  to a digital level with an *estimated* table for AirPods Pro at **100 %
  system volume**; set the volume to 100 % before starting, and use the
  *Level offset* slider if the 40 dB HL sample tone is clearly loud or barely
  audible. Levels below roughly 10 dB HL sit near the Bluetooth codec's noise
  floor and are not reliable.
- The AirPods must be the system's audio output (the A2DP sink). The tab
  shows which output device will be used.
- While connected, the app switches the earbuds to Noise Cancellation and
  turns Hearing Health off for the duration of the test, and restores both
  afterwards.
- Tones start at 40 dB HL, never exceed 80 dB HL, and the rendered level is
  hard-capped at about -6 dBFS whatever the offset. Remove the earbuds at once
  if anything is uncomfortably loud.

**Manual mode** (toggle on the same tab) is a quicker alternative: one slider
per band and ear (-10 to 80 dB HL in 5 dB steps, starting at 0). Hold a slider
and that band's beeps repeat in that ear, getting louder as you move right;
leave it where the beeps are only just audible, then press *Use in Audiogram
tab*. The level estimate, the offset slider and the caps are the same as for
the automatic test, and the earbuds are switched to Noise Cancellation with
Hearing Health off while the manual page is shown.

## Safety limits

- On first launch the app shows the medical/safety disclaimer above and stays
  disconnected from the earbuds until you acknowledge that you have read and
  accept it.
- Hard ceilings are enforced in `crates/airpods-proto` at the encoder — the
  last stop before the radio — so no code path can send larger values to the
  earbuds: **amplification** (including own-voice and per-ear after balance)
  is clamped to `-1.0..=1.5`, every **profile-curve band** to
  `0..=120 dB HL`, balance and tone to `-1.0..=1.0` and ambient noise
  reduction to `0.0..=1.0`. Non-finite values (NaN/inf from hand-edited
  JSON) become 0.
- In the UI, amplification above **1.00** (Apple's own maximum) is held at
  1.00 until you confirm a warning: values above that are outside Apple's
  limits and could damage your hearing and/or the AirPods. The UI rejects
  dB HL entries outside 0–120 instead of silently clamping them.

Start with low values, change one setting at a time, and never raise
amplification while the earbuds are at a loud listening volume.

## Trademarks

AirPods and AirPods Pro are registered trademarks of Apple Inc. This project
is independent and not affiliated with, endorsed by, or sponsored by Apple
Inc. Other product names are used only to describe hardware compatibility.

# Screenshots

<img width="600" height="820" alt="Status tab" src="screenshots/status.png" />
<img width="600" height="820" alt="Hearing Health tab" src="screenshots/hearing-health.png" />
<img width="600" height="820" alt="Audiogram tab" src="screenshots/audiogram.png" />
<img width="600" height="820" alt="Adjustments tab" src="screenshots/adjustments.png" />

## Layout

```
openaircare/
  crates/airpods-proto   pure protocol (AACP framing, control commands, ATT PDUs,
                         104-byte Hearing Health codec) - no I/O, unit tested
  crates/airpods-link    BlueZ L2CAP transport (Linux) + session state machine,
                         plus a `mock` fake-AirPods for development on any OS
  app/                   the Makepad UI
```

## Prerequisites (Linux)

1. **BlueZ** with `bluetoothd` running (any modern distro / Raspberry Pi OS).
2. Your AirPods paired and **connected** as an audio device the normal way
   (`bluetoothctl connect XX:XX:...` or the desktop Bluetooth settings).
3. **Vendor-ID spoofing.** AirPods only expose the Hearing Health channel to
   hosts that identify as Apple. The app's Setup tab sets this and pairs the
   earbuds (`./setup.sh` does the same and installs the runtime libraries).
   By hand, add to
   `/etc/bluetooth/main.conf` under `[General]`:

   ```ini
   DeviceID = bluetooth:004C:0000:0000
   ```

   then `sudo systemctl restart bluetooth` and **re-pair** the AirPods (they
   cache the host's Device ID). Without this the app still connects (battery,
   listening mode, the Hearing Health on/off switch) but the ATT channel on
   PSM 31 is refused (`Connection refused`), so audiogram/adjustments are
   unavailable and the Status page tells you so.
4. Only one AACP client per host: quit other programs that manage the airpods when using this one.
5. Build dependencies for Makepad (Ubuntu/Debian/Raspberry Pi OS):

   ```bash
   sudo apt-get install -y build-essential pkg-config clang libssl-dev \
     libx11-dev libxcursor-dev libxkbcommon-dev libxrandr-dev libxi-dev libxinerama-dev \
     libasound2-dev libpulse-dev libwayland-dev wayland-protocols \
     libegl1-mesa-dev libgl1-mesa-dev libgles2-mesa-dev libglx-dev libdrm-dev libgbm-dev \
     mesa-vulkan-drivers libdbus-1-dev
   ```

   (`libdbus-1-dev` is for the BlueZ D-Bus client used to find the AirPods.)

## Build & run

Rust stable (see `rust-toolchain.toml`). Makepad is pulled from git at a
pinned revision, so the first build takes a while.

```bash
cd openaircare
cargo test --workspace --features airpods-link/mock   # protocol + session tests
cargo run -p openaircare --release          # real BlueZ backend
cargo run -p openaircare --features mock    # fake AirPods, no hardware
```

Environment knobs:

| variable | effect |
|---|---|
| `RUST_LOG=debug` | log every AACP/ATT packet (hex) to stderr |
| `MAKEPAD_GPU=gl` | force the OpenGL ES backend if Vulkan misbehaves |
| `MAKEPAD=linux_direct` (build-time) | render straight to DRM/KMS without X11/Wayland (kiosk Pi) |
| `OPENAIRCARE_MOCK_FRESH=1` | with `--features mock`: start as a computer that was never set up, to try the Setup tab |
| `OPENAIRCARE_ATT_SCAN=1` | "Reload from AirPods" also reads ATT handles 0x0001-0x0060 and logs them (find where a firmware keeps a value; diff two runs) |

Settings (last audiogram / adjustments) are stored in
`$XDG_CONFIG_HOME/openaircare/` (defaults to `~/.config/...`).

## Raspberry Pi 5

Works the same; build natively on Raspberry Pi OS (64-bit, Bookworm or
later). The default Wayland session uses the V3D Vulkan driver; if the window
stays black try `MAKEPAD_GPU=gl`. On the X11 (openbox) session Makepad picks
the OpenGL ES 3.1 V3D backend by itself. A clean `--release` build takes
about 5 minutes on a Pi 5 (8 GB); the debug build of makepad about 10.

## Troubleshooting

- **`the AirPods rejected this host's pairing key` / `Invalid exchange (os error 52)`** -
  the buds no longer hold a link key for this host (HCI status `PIN or Key
  Missing`). This happens after a reset, after pairing them to another
  computer, or occasionally when the host's Device ID changes. Run
  `bluetoothctl remove XX:XX:...`, put the AirPods in pairing mode (lid open,
  hold the case button until the light blinks white) and pair again.
- **BlueZ shows the AirPods as connected only for a few seconds** - normal
  on hosts without a working A2DP/HFP setup (e.g. a bare Raspberry Pi): the
  audio profiles fail and BlueZ drops the link. The app does not need that
  link; it pages the buds itself when opening the AACP channel, and retries
  with back-off (2 s .. 30 s) while "Reconnect automatically" is on.
- **Sliders / audiogram have no effect** - the buds ACK every write of
  handle `0x2A` but silently discard it unless header byte 1 is `0x02`
  (a never-configured AirPods Pro 3 reads back `02 00 60 00`). The app forces
  `02 xx 64 00`; "Reload from AirPods" on the Audiogram tab reads the blob
  back so you can verify what the buds actually stored. Adjustments are only
  audible while Hearing Health is on, in Transparency mode, with the buds in
  your ears. Verified on AirPods Pro 3: a stem swipe changes exactly the
  amplification fields the sliders write (offsets 36 / 84), and a value
  written by the app is applied immediately.
- **The Hearing Health switch does not "confirm"** - AirPods Pro 3 on firmware
  8A apply `0x2C`/`0x33` but never echo them (older firmware does). The app
  therefore shows the value it sent; the buds report the real, persisted
  state in their initial dump on the next connect (Disconnect / Connect on
  the Status page to double check). The first enable also flips the
  "enrolled" flag on the buds permanently, like the Android app does.
- **`Connection refused` on PSM 31** - open the Setup tab: Bluetooth has to
  show as done, and the AirPods have to be paired after that step.
  If it *used* to work in this session, the buds are still holding the ATT
  slot of a previous client (another script, a killed app): the app retries
  PSM 31 five times, and if that is not enough, bounce the link once with
  `bluetoothctl disconnect XX:XX:... && bluetoothctl connect XX:XX:...`; the
  app reconnects on its own.
- **AirPods disconnect after a while when spoofing the Device ID** - known
  upstream quirk (the buds expect more Apple-specific traffic). The app
  reconnects automatically; when you are done configuring, press *Undo
  Bluetooth changes* on the Setup tab, or run `./setup.sh --undo` (or remove
  the `DeviceID` line and restart Bluetooth).
- **`Permission denied` creating the L2CAP socket** - run `./setup.sh` again.
  It grants the app Bluetooth access (`setcap`) and adds your user to the
  `bluetooth` group. The group applies the next time you log in. Move the
  app into your home directory if it is on a USB stick. Those disks ignore
  the Bluetooth permission the script sets on the file.
- **Wrong device picked** - the app connects to the first *connected* device
  advertising the AACP UUID `74ec2172-0bad-4d01-8f77-997b2be0722a`. Use the
  MAC field on the Status page to force a specific one.

## Shoutout
Librepods - check out their repo https://github.com/librepods-org/librepods
