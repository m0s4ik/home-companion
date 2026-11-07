<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Installing home-companion

For a Lenovo ThinkSmart View running [thinksmart-view-debian](https://github.com/m0s4ik/thinksmart-view-debian)
v0.3-rc3 or later (Debian 13 arm64 with its `mesa-freedreno`). Build the system image with `WEB=1` for Home
Assistant's view on the screen and `AIRPLAY=1` for the AirPlay receiver; both are optional and found by the
installer. The device must already be on the network: Wi-Fi is set when the image is built (`WIFI_SSID`,
`WIFI_PSK` in its config.env). To change it later, on the device:
`nmcli device wifi connect "<ssid>" password "<psk>"` (from the console, or over ssh knowing the session drops).

```sh
scp home-companion-<version>-arm64.tar.gz root@<device>:/tmp/
ssh root@<device>
cd /tmp && tar xzf home-companion-<version>-arm64.tar.gz && cd home-companion-<version>-arm64
./install.sh
```

It asks nothing (only, on a re-install, whether to keep the configuration: Enter keeps it). It installs the
Debian packages it needs (never removing any: if apt wanted to replace `mesa-freedreno` it stops), downloads the
speech models of the reflexes for every language (~50-60 MB each), installs everything under `/opt/home-companion`,
writes `/etc/home-companion/home-companion.toml` and `brain.toml`, and starts the services. AirPlay and Home
Assistant's view are switched on when the system has what they need. The language and Home Assistant's address are
chosen on the device's screen at first start, or given here:
`./install.sh --language it --ha-url http://192.0.2.10:8123 --name Alfred`. `./install.sh --help` lists every flag;
`--dry-run` shows what it would do.

Then, on the device's screen, **Altro → Home Assistant** shows a QR code: open it with a phone logged in to
Home Assistant and confirm. The device links itself and sets up the rest in Home Assistant (the Wyoming
satellite, its tools, the conversation agent, the voice pipeline). Say "Okay Alfred".

- **Upgrade**: unpack the newer bundle and run its `install.sh`; the configuration and the state are kept
  (on a terminal it asks whether to keep the configuration: Enter keeps it) and the services restart.
- **Remove**: `./install.sh --uninstall` (configuration and state stay), `--uninstall --purge` (everything).
- **Log**: `journalctl -u home-companion -f`. Configuration keys and defaults:
  `/opt/home-companion/home-companion --print-config`.

Licences: `LICENSE` (GPL-3.0-or-later) and `THIRD-PARTY.md`.
