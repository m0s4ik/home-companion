# Installing home-companion on the device

For the ThinkSmart View running [thinksmart-view-debian](https://github.com/m0s4ik/thinksmart-view-debian) v0.3-rc3 or
later: Debian 13 arm64 with its Mesa (`mesa-freedreno`, which nothing here may replace), ALSA, avahi and a firewall
that includes `/etc/nftables.d/*.nft`. Optional layers of the image: `WEB=1` (WPE WebKit 2.54, for "Casa", Home
Assistant's view) and `AIRPLAY=1` (the GStreamer runtime UxPlay needs). The device must already be on the network:
Wi-Fi is set when the image is built (`WIFI_SSID`/`WIFI_PSK` in its config.env); to change it later, on the device,
`nmcli device wifi connect "<ssid>" password "<psk>"` (from the console, or over ssh knowing the session drops).

## The release bundle

```sh
WEB_HELPER=<arm64 home-companion-web> tools/release/build-bundle.sh 0.1.0     # -> out/release/home-companion-0.1.0-arm64.tar.gz
```

[tools/release/build-bundle.sh](../tools/release/build-bundle.sh) cross-builds and strips `home-companion` (`fbdev,gpu`),
`home-companion-brain` and `home-companion-reflex` (with libvosk 0.3.45 from `crates/spike-native/tools/fetch-vosk.sh`),
builds UxPlay with `tools/airplay/build-uxplay.sh` (or takes `UXPLAY=<file>`), and puts them with the character, the
locales, this directory's units, firewall and udev files, the configuration templates, `install.sh`,
`tools/slim-vosk-model.py`, [INSTALL.md](INSTALL.md), the licences and `THIRD-PARTY.md` (every Rust crate inside with its
licence; the UxPlay and femtovg patches in `source/`) into one tree with a `SHA256SUMS`, then refuses to finish if a
text file holds a private-looking address. The web helper (`web/`, C) needs `libwpewebkit-2.0-dev` 2.54 for arm64,
which the amd64 cross setup does not have: it is built where that exists (`make -C web/home-companion-web` on an arm64
Debian 13 with the WPE backport) and passed as `WEB_HELPER`; the script builds it itself when the arm64 pkg-config file
is found. The Vosk language models are not in the bundle (50-90 MB each): `install.sh` downloads them.

## install.sh

On the device, as root, from the unpacked bundle ([INSTALL.md](INSTALL.md) is the short version):

```sh
./install.sh                                   # nothing is asked: language and Home Assistant on the screen
./install.sh --language it --ha-url http://192.0.2.10:8123 --name Alfred
./install.sh --dry-run                         # what it would do, nothing changed
./install.sh --uninstall [--purge]
```

| Option | |
|---|---|
| `--language it\|en\|nl` | the language; without it the line stays commented out and the screen asks at first start |
| `--name NAME` | the satellite's name in Home Assistant and in the AirPlay menu (default `Alfred`) |
| `--ha-url URL` | Home Assistant's address; without it the screen finds it at first start (mDNS, `avahi-browse`). An IPv4 address also goes into `[satellite] allowed`, so a Wyoming integration added by hand works before the QR link |
| `--no-airplay` / `--airplay` | AirPlay is on when the system has its runtime (the GStreamer plugins and libraries of `AIRPLAY=1`); `--airplay` installs the missing Debian packages |
| `--no-web` / `--web` | "Casa" is on when WPE WebKit ≥ 2.54 is installed and Home Assistant's address is known (the body refuses `[web] enabled` without `[home_assistant] url`) |
| `--no-reflexes` | no Vosk models, `[reflexes] enabled = false` |
| `--keep-config` / `--new-config` / `--yes` | an existing configuration is kept (asked on a terminal, Enter keeps it; `--yes` keeps it) or written anew (the old one as `*.prev`) |

What it does, in order (a step already done is skipped, so the same run is the upgrade):

1. Checks the system: arm64, Debian 13, systemd, `mesa-freedreno` installed and `libgbm.so.1`; looks for WPE and the
   AirPlay runtime.
2. Debian packages: `libasound2t64 libdrm2 libstdc++6 libgcc-s1 libatomic1 python3 curl ca-certificates nftables
   avahi-daemon avahi-utils`, plus the AirPlay runtime with `--airplay`. Missing ones are simulated first
   (`apt-get -s`): if apt would remove anything (`Remv`, e.g. `mesa-freedreno` for Debian's Mesa) it stops, else
   `apt-get install --no-install-recommends`.
3. Users and groups, system users without a login: `home-companion` (also in `home-companion-brain` and
   `home-companion-web`, to reach their sockets), `home-companion-brain`, `home-companion-web`. The device groups
   (`audio video input tty`, `render`) come from the units' `SupplementaryGroups`.
4. Files: each binary is installed next to the old one and renamed over it (a running service keeps its copy);
   `characters/` and `locales/` are replaced whole; the web helper only when WPE is there.
5. The Vosk model of every language that has a grammar (`characters/*/grammar/<lang>.toml`, so the language can be
   chosen on the screen; `VOSK_MODELS` in install.sh gives each model and its sha256): downloaded from
   alphacephei.com, checked, unpacked, slimmed with `slim-vosk-model.py` (the reflexes decode a closed grammar:
   `graph/Gr.fst` goes, about 50 MB less RAM) into `/opt/home-companion/reflex/<lang>`, with a `.model` file naming
   it. Only the active language's model is loaded; the others cost disk only.
6. `/etc/home-companion/home-companion.toml` and `brain.toml` from [home-companion.toml.in](home-companion.toml.in)
   and [brain.toml.in](brain.toml.in), only when missing (or `--new-config`); mode 640, group `home-companion` and
   `home-companion-brain`.
7. The units, the udev rule (`udevadm trigger -s backlight`), the firewall file (`nft -c -f /etc/nftables.conf`,
   then `systemctl reload nftables`; a file the firewall rejects is put back as it was).
8. `ldd` on the installed binaries (a warning for a missing library), `/opt/home-companion/VERSION`, `systemctl
   enable` and start (or restart, on an upgrade; `home-companion-web` restarts with the body, `PartOf`).
9. Prints the next step: Altro → Home Assistant on the screen shows the QR code; after the link the device sets up
   Home Assistant by itself (Wyoming satellite, MCP tools, agent, pipeline).

`--uninstall` stops, disables and removes the units, the udev rule, the firewall file and `/opt/home-companion`
(the downloaded models too); `/etc/home-companion`, the state and the users stay unless `--purge`. The Debian
packages it installed stay.

## Reference: what goes where

| Path | What |
|---|---|
| `/opt/home-companion/home-companion` | the body (`cargo build --release --target aarch64-unknown-linux-gnu -p home-companion --no-default-features --features fbdev,gpu`) |
| `/opt/home-companion/characters/`, `/opt/home-companion/locales/` | the character (with its wake and stop word models) and the on-screen text |
| `/opt/home-companion/VERSION` | the installed bundle's version and commit |
| `/etc/home-companion/home-companion.toml` | configuration (`home-companion --print-config` shows every key with its value) |
| `/var/lib/home-companion/` | state (timers, settings, the QR link's tokens in `ha-auth.json`, AirPlay), created by systemd (`StateDirectory`) |
| `/etc/systemd/system/home-companion.service` | [home-companion.service](home-companion.service) |
| `/etc/nftables.d/home-companion.nft` | [home-companion.nft](home-companion.nft): the link, the satellite, the brain and AirPlay, from the LAN only |
| `/etc/udev/rules.d/99-home-companion-backlight.rules` | [99-home-companion-backlight.rules](99-home-companion-backlight.rules): group `video` may write the backlight |
| `/opt/home-companion/airplay/uxplay` | AirPlay (music and screen mirroring): UxPlay 1.73.7 with our patch, built by `tools/airplay/build-uxplay.sh`; needs the runtime listed under "AirPlay" below and `[airplay] enabled = true` |
| `/opt/home-companion/home-companion-reflex`, `/opt/home-companion/lib/libvosk.so` | the reflex helper (`cargo build --release --target aarch64-unknown-linux-gnu -p reflex --features vosk --bin home-companion-reflex`) and the prebuilt libvosk 0.3.45 (Apache-2.0, `crates/spike-native/tools/fetch-vosk.sh`; the helper finds it through its rpath `$ORIGIN/lib`, it needs `libatomic1`) |
| `/opt/home-companion/reflex/<lang>/` | the Vosk model of each language with reflexes: `it` = [vosk-model-small-it-0.22](https://alphacephei.com/vosk/models), `en` = vosk-model-small-en-us-0.15, `nl` = vosk-model-small-nl-0.22, all Apache-2.0, slimmed (`graph/Gr.fst` goes, its word table stays as `graph/words.txt`). A language without a model has no reflexes (one log line); `[reflexes] enabled = false` turns them off |
| `/opt/home-companion/home-companion-web`, `/etc/systemd/system/home-companion-web.service` | optional, for "Casa": the web helper (`web/home-companion-web`) and [its unit](home-companion-web.service); its profile in `/var/lib/home-companion-web`, cache in `/var/cache/home-companion-web` (`StateDirectory`, `CacheDirectory`) |
| `/opt/home-companion/home-companion-brain`, `/etc/home-companion/brain.toml`, `/etc/systemd/system/home-companion-brain.service` | the brain (below); memory in `/var/lib/home-companion-brain` |

## How it runs

home-companion runs as the `home-companion` user, not root: `audio` for ALSA, `video` for `/dev/dri`, the framebuffer and (with the udev
rule) the backlight, `input` for touch and keys, `tty` plus `CAP_SYS_TTY_CONFIG` to switch the console to graphics
mode while it draws. The accelerometer (`/sys/bus/iio/devices/*/in_accel_*_raw`) is readable by everybody.
Settings → Suspend puts the whole device to sleep (s2idle; Vol+ or Vol− wakes it, touch does not): the body writes
`mem` to `/sys/power/state` itself, and before that `+N` to `/sys/class/rtc/rtc0/wakealarm` when a timer is pending,
so the RTC wakes the device 10 s before it rings. No polkit and no sudo: a second `ExecStartPre=+` in the unit (like
the governors) gives group `home-companion` write access to both files at every start (sysfs modes do not survive a reboot).
`Environment=HOME_COMPANION_SUSPEND_DRY_RUN=1` (e.g. in a drop-in) checks both files and logs what would be written instead.

Screen behaviour (`[screen]` in the configuration, defaults shown by `home-companion --print-config`): the home screen is dim
(`idle_brightness`, 600 of 4095) with the character still; a touch, a volume key, moving the device or the end of a
conversation makes it bright (`active_brightness`) for `awake_seconds` (60), while the character plays its wake-up
and idle animations. Every other screen is bright. Vol+ and Vol− pressed together outside "Casa" put the screen to
sleep at once (backlight 0, home-companion still listening) until the next touch, key, movement or conversation; a key press that
only wakes the screen (or the device, after Settings → Sospendi) does not change the volume. Without the udev rule the
levels are only logged. To wake it by hand: `systemctl kill --kill-whom=main -s USR1 home-companion`; a screenshot of
what is on the screen (Home Assistant's view included): `systemctl kill --kill-whom=main -s USR2 home-companion`, the PNG goes to
`[ui] screenshot_dir` (`/var/lib/home-companion/screenshots`, the last 20 are kept). `--kill-whom=main`: the signal must not
reach the reflex helper.

Automatic brightness (`auto_brightness`, on): with the VCNL4200 light sensor the two levels follow the room's light on
a curve through `auto_dark`, `auto_room` and `auto_bright` (`[lux, idle, active]`; by default 0.2 lux → 40/400,
20 lux → 600/2200, 100 lux → 1200/4095, in the sensor's own lux: behind the dark bezel glass it reads about ten times
less than the room), smoothed and with hysteresis, fading over 1.5 s. home-companion reads the sensor's
light part directly on its I²C bus every 2 s (the unit's `ExecStartPre` gives group `home-companion` the bus), so the proximity
IR LED stays off (reading `in_illuminance_raw` through the driver would switch it on). Night mode (Settings →
Schermo, 23-07 by default) caps the levels at `night_brightness` / `night_active_brightness` (40 / 400).

"Casa" (`[web]`, off by default): the Home Assistant button opens Home Assistant's own frontend, drawn by the web
helper `home-companion-web` (its unit: `home-companion-web.service`, above). The body connects to the helper's socket
(`[web] socket`, `/run/home-companion-web/home-companion-web.sock`) and never waits for it. `home-companion-web.service` runs the helper as its own user
`home-companion-web` (render node, its profile in `/var/lib/home-companion-web`, nothing of the body's), starts and stops with the body, and
is skipped while `[web] enabled` is false; the login is passed once on standard input, never on a command line. While "Casa" is visible the body sets the CPU governor to `[web] governor`
(`performance`; the unit's `ExecStartPre` lets group `video` write it) and puts the previous one back when it is
hidden; after `[web] freeze_after_seconds` hidden it freezes the helper's cgroup, which must be the helper's own unit
(`home-companion-web*`) with `cgroup.freeze` writable by `home-companion`; `cgroup.kill` too, so that after a link by QR code the body
starts the helper again with the new tokens (in home-companion-web.service: `ExecStartPost=+/bin/chown home-companion
/sys/fs/cgroup/system.slice/home-companion-web.service/cgroup.freeze /sys/fs/cgroup/system.slice/home-companion-web.service/cgroup.kill`).
The link page needs `[home_assistant] link_port` (8124) open from the LAN. The helper logs in with
`home-companion web-login | home-companion-web --token-stdin --url "$(home-companion web-url)" ...` (Home Assistant's address and token from
`[home_assistant] url` and the body's tokens, the page from `[web] path`; the token never on a command line). Ways back
home: the round button (bottom right, shown for 4 s after a touch, `[web] home_button_seconds`; it can be dragged
to any edge and its place is kept in `<state_dir>/settings.toml`), a swipe up from the bottom edge, Vol+ and Vol−
pressed together (outside "Casa" the pair puts the screen to sleep instead). Left alone it stays open, so it can be
kept as a wall dashboard (`[web] idle_return_seconds` > 0 sends it home after that long untouched); it gives way
when a conversation or a timer needs the screen and comes back afterwards. For tests
without the unit (a test root): `web/test/helper.sh`.

Linking to Home Assistant (Settings → Home Assistant, and the first start without a token): the page shows a QR code
with Home Assistant's authorisation page; the phone opens it, the user confirms, and Home Assistant sends the phone
back to the body on `[home_assistant] link_port` (8124), where the body exchanges the one-time code for an access and
a refresh token, kept in `/var/lib/home-companion/ha-auth.json` (mode 600) and refreshed by itself. The body listens on that
port **only while the page is on screen** and closes it after the link; the firewall must let the LAN reach it
(`home-companion.nft` does). The device appears among the
user's sessions in Home Assistant's profile (Security → Refresh tokens), where it is revoked; a revoked link removes
the file and the body says "not linked". Without a link the long-lived token of `[home_assistant] token_file` is used
as before. After linking, "Casa" logs in with the new tokens at the helper's next start (`systemctl restart
home-companion-web`, or a reboot); until then it keeps the login it started with.

## AirPlay (optional)

The body runs UxPlay as its child while AirPlay is on (`[airplay] enabled`, and the switch in the control centre):
music goes through the body's own mixer (UxPlay never opens ALSA), a mirrored screen's frames are decoded by the
hardware decoder (venus, V4L2) and handed to the body as dma-bufs over a socket (`unixfdsink`), and the body draws
them. Nothing runs while AirPlay is off.

The runtime is thinksmart-view-debian's `AIRPLAY=1` layer (or `install.sh --airplay`, which installs
`gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-libav libgstreamer-plugins-bad1.0-0 libplist-2.0-4
libavahi-compat-libdnssd1`, and the whole `gstreamer1.0-plugins-bad` when the image's small
`gstreamer1.0-plugins-bad-airplay` is not there).

What each package is for: `libplist-2.0-4` and `libavahi-compat-libdnssd1` (with the system's `avahi-daemon`) are
UxPlay's own libraries; GStreamer core (`libgstreamer1.0-0`, pulled in) gives `fdsink` and `queue`; `-base` `appsrc`,
`audioconvert`, `audioresample`, `volume`, `videoconvertscale`; `-good` `v4l2h264dec` (venus) and `capssetter`;
`-bad` `h264parse` and `unixfdsink`; `-libav` the AAC and ALAC decoders. GStreamer's GL and ALSA plugins are not
needed (the body draws and plays). Ports: mDNS (UDP 5353, already open in the system image) and UxPlay's TCP
6010-6011 and UDP 6010-6012 ([home-companion.nft](home-companion.nft); `[airplay] port` moves them). The sender's remote (pause, next,
volume) is DACP, resolved through avahi-daemon's D-Bus API with `busctl` (systemd). State (pairing key, cover art,
the video socket) is in `/var/lib/home-companion/airplay/`.

## The brain (`home-companion-brain`, optional)

Tools for Home Assistant's own AI: the conversation agent is an agent
of whatever AI integration the user set up in Home Assistant (Google Gemini, OpenAI, Anthropic…, its key stays there);
`home-companion-brain` serves it the device's tools over MCP (Home Assistant's built-in MCP client): memory, reminders, timers and
alarms on the device, the shopping list, the house with gates. Long-term memory stays on the device
(`/var/lib/home-companion-brain`: `brain.db`, `profile.md`, `notes.md`; the two Markdown blocks can be edited by hand); the memory
jobs think through Home Assistant's `ai_task`. No API key on the device. (The earlier way, a model called directly with
a key on the device and a Wyoming `handle` service, is kept on the branch `standby/brain-direct-llm`.)

| Path | What |
|---|---|
| `/opt/home-companion/home-companion-brain` | the brain (`cargo build --release --target aarch64-unknown-linux-gnu -p brain`, ~5 MB, ~7 MB RSS) |
| `/etc/home-companion/brain.toml` | configuration: [brain.toml.in](brain.toml.in) (`home-companion-brain --print-config` for all keys) |
| `/etc/home-companion/brain-ha-token` | optional: the body hands the brain its own Home Assistant token (the QR link's) over the control socket every few minutes; this long-lived token, mode 600, owned by `home-companion-brain`, is used only when the body has not |
| `/opt/home-companion/characters/<name>/persona.<lang>.md` | the character's persona, for the agent's instructions |
| `/etc/systemd/system/home-companion-brain.service` | [home-companion-brain.service](home-companion-brain.service) |

Firewall: the brain's port (`listen`, default 10701) from Home Assistant only. In Home Assistant (the body will do this
by itself after the QR link; until then by hand): Settings → Devices & services → Add integration → Model Context
Protocol, URL `http://<device>:10701/mcp`; a conversation agent under the AI integration with Alfred's tools only (not
"Assist", so the gates cannot be bypassed) and the character's instructions; a pipeline for the device with that agent
and "Prefer handling commands locally" off, chosen on the device's satellite. With `allow` empty only Home Assistant's
own address (from `home_assistant.url`, or the addresses the body learned after the QR link) may connect. `[safety]`: alarms, locks and door, gate and garage covers (and
switches named like them) are never controlled by voice; other covers and valves only after a spoken yes the satellite
heard. Home Assistant reads the tool list at setup and every 30 minutes: reload the MCP integration after an update.

The body's control socket (`control_socket`, `/run/home-companion-brain/brain.sock`): the "Ricordi" page (what the brain knows,
each fact deletable, "Dimentica tutto") and the reminder cards. The unit's `RuntimeDirectory` is group `home-companion-brain`, so
the body's user is in that group (install.sh). Without the brain (no socket) the page
says the brain is not active and the Altro tile is hidden.

Nightly tidy-up (`[memory] consolidate_at`, "03:30"; "" = off): with Home Assistant's AI task the brain merges facts that say the
same thing, updates changed ones, retires the past ones (an appointment last week) and shortens the two blocks within
their limits, in one transaction; skipped while a conversation is open or ended less than 10 minutes ago; after a
suspend it runs at the next wake-up within 6 hours, else that night is skipped. The result (counts only) is on the
"Ricordi" page and in the journal (`event=brain_tidy`).

Reminders (`schedule` tool, "ricordamelo alle 18", "svegliami alle 7"): a card on the home first (OK = done); if it is
not touched within `[jobs] voice_after_seconds` (120) Home Assistant says it on the device
(`assist_satellite.announce` on `[jobs] satellite_entity`; empty = cards only), never inside the quiet hours
(`quiet_start`-`quiet_end`, 23:00-07:00: the voice waits for their end), except a "svegliami" (`how = voice`), spoken at
once. Up to an hour late (the device was off or suspended) a reminder is shown marked late; older ones become one
"missed" card. Daily or weekly repeats; at most `max_active` (20) pending. The morning summary (`[briefing] at`, off by
default; `weather_entity`, `calendars`) is a card; "leggimi il riepilogo" reads it.

Shopping list: `[lists] shopping = "todo.<entity>"` in **both** `/etc/home-companion/home-companion.toml` (the "Spesa" card on the home with
the count and the first items, the Altro tile and the page where a tap ticks an item; live, through
`todo/item/subscribe`, so the phone's changes show at once) and `/etc/home-companion/brain.toml` (the voice: `list_add` with
several items and their quantities, no duplicates, a ticked item put back; `list_read`, ten items at most;
`list_done` "ho preso il pane"; `list_remove`). Any `todo` entity works (Home Assistant's own shopping list, Bring!,
…); empty = everything hidden. Home Assistant's own sentences ("aggiungi il latte alla lista della spesa") keep
being handled by its local intents before the brain.

