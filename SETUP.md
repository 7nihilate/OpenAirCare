# OpenAirCare setup (Linux)

Download a release, unpack it, and open the app. Its **Setup** tab configures Bluetooth so AirPods will open Hearing Health, and pairs them.

OpenAirCare is not a medical device. Read the safety notice in [README.md](README.md). The app asks you to accept it before connecting.

## Setup

1. Open [Releases](https://github.com/brettpreston/OpenAirCare/releases) and download the archive for this computer: `x86_64` for most PCs, `aarch64` for a Raspberry Pi or other ARM board. (A build from Actions uses the same names.)
2. Unpack it and open `openaircare` (double-click it, or from a terminal):

```bash
cd ~/Downloads
tar xzf openaircare-*-linux-*.tar.gz
cd openaircare-*-linux-*
./openaircare
```

3. Accept the safety notice. The **Setup** tab opens when something is still to do:

   - **Set up Bluetooth** asks for your password, sets the Apple vendor ID Bluetooth has to advertise (`DeviceID = bluetooth:004C:0000:0000`) and restarts Bluetooth. Bluetooth mice, keyboards, and headphones drop for a few seconds.
   - **Search for AirPods** pairs them. First put them in the case, leave the lid open, and hold the button on the back of the case until the light flashes white. AirPods that were paired before the Bluetooth step need **Remove pairing** and pairing again.
   - **Add to app menu** puts OpenAirCare in the app menu.
   - **Undo Bluetooth changes** puts Bluetooth back how it was before setup.

4. Wear the AirPods and press **Connect** on the Status tab.

If the window does not open, a library is missing: use the terminal setup below.

## Setup from a terminal

`./setup.sh` does the same steps in a terminal and also installs missing libraries (Debian, Ubuntu, Raspberry Pi OS). Use it when the app does not start, or when the desktop has no password prompt.

```bash
./setup.sh                # install libraries, set up Bluetooth, pair, start the app
./setup.sh --no-launch    # set up, but do not start the app
./setup.sh --no-pair      # set up, but skip pairing
./setup.sh --undo         # put Bluetooth back how it was before setup
```

`--undo` removes the Apple vendor ID and restarts Bluetooth. Pair the AirPods again if they stop connecting. The downloaded app stays where it is.

## Troubleshooting

| Problem | What to do |
|---|---|
| `no paired AirPods found` | Pair them on the Setup tab. The lid stays open and the light flashes white. |
| `Connection refused` on the Hearing Health channel | On the Setup tab, check that Bluetooth shows as done, then remove the pairing and pair the AirPods again. Quit any other AirPods app first. |
| `Permission denied` | Run `./setup.sh` again. If this folder is on a USB stick or another protected disk, move it into your home directory and run setup from there. |
| AirPods drop after a few seconds | Turn on **Reconnect automatically** in the app. |
| Black or blank window | The app menu entry already works around this. From a terminal: `MAKEPAD_GPU=gl ./openaircare` |
| Missing library, or the app does not start | Run `./setup.sh`. |

## Manual setup

Use this when neither the Setup tab nor `./setup.sh` can run (no administrator password, or a distribution without `apt`).

Install the runtime libraries (on older Ubuntu or Debian, use `libasound2` if `libasound2t64` is not found):

```bash
sudo apt-get update
sudo apt-get install -y bluez libcap2-bin libdbus-1-3 libasound2t64 libpulse0 \
  libx11-6 libxcursor1 libxkbcommon0 libxrandr2 libxi6 \
  libwayland-client0 libwayland-cursor0 libwayland-egl1 \
  libegl1 libgl1 libgles2 libgbm1 libdrm2
```

Allow the app to open Bluetooth sockets, and add your user to the `bluetooth` group (log out and back in so the group applies):

```bash
sudo setcap cap_net_raw,cap_net_admin+eip ./openaircare
sudo usermod -aG bluetooth "$USER"
```

AirPods only open the Hearing Health channel to an Apple host. Under `[General]` in `/etc/bluetooth/main.conf`, set:

```ini
DeviceID = bluetooth:004C:0000:0000
```

Then restart Bluetooth and check the identity. The Modalias line should contain `v004C`:

```bash
sudo systemctl restart bluetooth
bluetoothctl show | grep Modalias
```

Pair the AirPods after that change. If they were paired before, remove them first (`bluetoothctl remove XX:XX:XX:XX:XX:XX` or the desktop Bluetooth settings). Put them in the case, lid open, and hold the case button until the light flashes white. Pair in **Settings → Bluetooth**, or:

```bash
bluetoothctl power on
bluetoothctl scan on
bluetoothctl pair XX:XX:XX:XX:XX:XX
bluetoothctl trust XX:XX:XX:XX:XX:XX
bluetoothctl connect XX:XX:XX:XX:XX:XX
```

Start the app from the unpacked folder:

```bash
MAKEPAD_GPU=gl ./openaircare
```

When you are finished configuring and do not want the computer to keep identifying as Apple, remove the `DeviceID` line and run `sudo systemctl restart bluetooth`. `./setup.sh --undo` does this if setup saved the previous file.
