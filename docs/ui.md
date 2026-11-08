<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# The body's screen: the quiet desk

The visual system of the companion screen and the specification of what is built:
`crates/spike-ui/ui/companion.slint` (the screens), `crates/spike-ui/ui/casa.slint` (Home Assistant's view),
`crates/spike-ui/ui/design/` (tokens, components, icons) and `crates/home-companion/src/ui/` (the data). Golden
screenshots of every screen: `crates/home-companion/fixtures/golden/` (`home-companion --simulator --state <name>`,
the names are in `crates/home-companion/tools/shoot-golden.sh`). The rest of the design is in [design.md](design.md).

The words quoted below are the English locale's (`locales/en.toml`); every language has its own file.

## 1. Direction

1. **Quiet desk**: the character keeps his stage on the left (x 0-512) and nothing is ever drawn over him; everything
   else lives on his desk, one column on the right, in three fixed bands (top, middle, action band). The stage and the
   bands never move, so a button, a card or a title is always where the eye expects it.
2. The home is calm (a big light clock, one line "date · sky temperature", one gold round "Talk"); it scales because
   **living things arrive as cards** (at most three) and **everything else is one tap away in "More"**, a desk page,
   not a full-screen launcher.
3. **Every screen has the same action band**: three slots, the middle one is the only gold button (Talk, Stop,
   Retry); the dock is **HA · Talk · More** ("HA" on the dock and tight labels, "Home Assistant" in titles and
   sentences).
4. **Home Assistant is the only full-screen view**, with three ways back (nothing on top at rest; a tap shows the
   indicator and the home button for 4 s; the swipe in from the button's edge and Vol+ with Vol− always work).
5. **Icons are filled Material Design Icons drawn as SVG images**, never as vector paths, sized for the distance they
   are read from.

## 2. Layout

The panel is 1280 x 800 (landscape).

- **Stage** x 0-512: the pose 740 px tall, top 30, centred at x 256. A tap wakes the screen; a double tap on the
  mouth opens the settings.
- **Desk column** x 560-1232: 672 px, centre 896; 48 px from the right edge and from the stage.
- **Bands** (y): page header 32-96 (back chevron + title, the time small at the right); home time band 48-290 (clock
  at 48, date line at 240); **middle band 312-600** (home: three 88 px cards, 12 px apart; pages: 112-608, their
  content **scrolls inside the band**, the header and the action band never move); **action band 640-776**: three
  160 x 136 slots centred at x 696, 896, 1096.
- Screen-level actions only in the action band; actions on an item inside its card or row. A slot without a job
  stays empty. 8 px grid, whole-pixel positions (icons are drawn 1:1). Dialogs sit on the desk, or in the middle of
  the screen over Home Assistant's view.
- **Every target is at least 64 x 64** (bare actions: the whole 160 x 136 slot; the switch: 96 x 64 around its
  72 x 40 track; the home button over Home Assistant: 96 px).

## 3. Tokens (`design/tokens.slint`)

| Token | Value | Use |
|---|---|---|
| `bg` | `#03050d` | the character's background (poses are opaque pictures on it; their plates are flattened to it at load) |
| `surface` / `surface-2` | `#111318` / `#1b1c22` | cards, tiles, groups, dialogs, the HUD / pressed, the user's chat bubbles |
| `line` | `#2a2824` | 1 px borders, separators, tracks |
| `text` | `#efe9df` | clock, titles, body (16.9:1) |
| `text-2` | `#a39b8f` | date, second lines, values, bare icons, action labels (7.4:1) |
| `text-3` | `#8a8479` | hints, captions, section titles, disabled (5.5:1 on bg, 5.0:1 on surface) |
| `accent` / `accent-deep` / `on-accent` | `#d4ab63` / `#35281a` / `#1f170c` | the one primary per screen, the marks of living things |
| `danger` | `#e07a5f` | the body's own problems (no connection, a view that cannot open) |
| `ok` / `warn` | `#8cc084` / `#e6c15a` | the setup's checks (Settings → Home Assistant) |
| `night` | `#5c5750` | the night clock and its line |
| `scrim` | `#000000b3` | behind a dialog |

The conversation colours (listening, thinking, speaking, error) stay **the character's** (`character.toml`); the body
uses them for the wave and the Stop ring only.

**Type** (Inter; eight sizes): 160 Light (the clock, Inter Tnum), 96 Light (a finished timer's name, the start-up
name), 48 (state phrases, dialog digits), 40 (notice and music titles; countdowns and alarm times in Inter Tnum), 34
(page titles, the answer, the date line), 28 (card titles, rows, tiles), 24 (chat, hints, second lines, action
labels, Medium for labels), 20 Medium tracked (section titles, timestamps). Light only at 160 and 96: thinner stems
go grey from across the room. **Inter Tnum** is Inter with the digits' cmap pointed at Inter's own tabular figures
(Slint has no font-feature API; without it the clock would shift every minute).

## 4. Icons

Filled MDI glyphs as SVG files, one per size (`design/icons/<size>/<name>.svg`, made by `tools/make-icons.py`, listed
in `design/icons.slint` as `Icons.<name>-<size>`), drawn as `Image { colorize }`. femtovg fills MDI paths badly at
these sizes (thin holes covered by anti-alias fringes); resvg rasterizes them cleanly, once per size, and the GPU
draws a textured quad. Sizes: 48 the action band and tiles, 44 inside the gold disc, 40 the page header, 32 cards and
weather, 28 rows and the HUD, 24 inside the timer ring. Glyphs: dock `view-dashboard` (HA), `microphone`,
`dots-horizontal` (More); `home` only means "back to the character" (the button over Home Assistant);
`timer-sand-full` for a heavy view, `plus` for "+1 minute", `timer-plus`/`alarm-plus` for new timers and alarms.
`Path` stays only for shapes that are data (the timer ring).

## 5. Components (`design/components.slint`)

`Icon`, `BareButton`, `PrimaryButton` (96 px disc; `stop` draws the square; Stop takes the state colour),
`PillButton` (64 px; gold only in dialogs), `RingButton`, `GlyphButton`, `Toggle`, `TimerRing` (empties clockwise from
12 o'clock, one short path a second), cards (`Card`, `InfoCard`, `TimerCard`, `NowPlayingCard`), `PageHeader`,
`Tile`, `SectionTitle`, `Group` + `Row` (chevron, check or switch), `Stepper`, `Chip`, `TimerLine` (24 px between
pause and cancel), `AlarmLine`, `ChatUser` / `ChatCharacter` (as tall as their text) / `ChatSeparator`, `Step`,
`Dialog` (scrim, icon, title, text, content, Cancel ring + gold action), `VolumeHud`, `ControlTile` and `LevelBar`
(the control centre). No shadows, no blur, no gradients over content (one 48 px fade at the top of the chat list).

## 6. Screens

- **Home**: clock; "Wednesday 4 November · ⛅ 14°" (date in the locale's case, temperature rounded, no unit, in
  `text`; the glyph and the temperature disappear together without data from Home Assistant). Nothing alive: the
  character's idle phrase (Say "Okay Alfred") in `text-3`. **Cards**, at most three, most urgent first
  (`crates/home-companion/src/ui/cards.rs`): (1) the body's status ("Home Assistant is not answering · I retry by
  myself · timers and alarms work"), then the setup in Home Assistant when it needs the user, (2) due reminders that
  need an OK ("1 of 3" when more wait), then a screen mirroring left for the home, (3) timers, soonest first, at most
  two (more become one "+N timers" card that opens the timers page), (4) the next alarm within a day, (5) the next
  reminder, (6) the shopping list, (7) what plays, (8) the last long answer for a few minutes (opens Conversations).
  A tap on a card opens what it belongs to.
- **Offline** (Home Assistant not connected): the offline pose, the status card, HA dimmed to 35 %; Talk still works
  for the local reflexes.
- **Conversation**: the wave (72 bars, the state colour), the phrase 48, the heard sentence while thinking and small
  above the answer while speaking, the answer 34 up to five lines (the whole text is in Conversations). Stop takes
  Talk's place; in **error** ("I didn't understand", "Say it again? Just say «Okay Alfred»") the gold Talk stays, to
  try again; home after 6 s. A conversation is drawn over any page and leaves it as it was.
- **Timer or alarm finished**: takes over everything (Home Assistant's view too, which comes back afterwards):
  "Timer finished" (or "Alarm") in gold, the name 96 Light, the time since it ended counting up, "Touch anywhere to
  stop"; band "+1 minute" (a new local timer of one minute with the same name) and Stop, with a ring that breathes.
  The character uses `timer_finished`, falling back to `speaking`.
- **More**: tiles Conversations, Timers and alarms, Settings and Home Assistant, plus Music while something plays,
  Memories and Reminders when the brain answers, Shopping when a list is configured; each says what is going on
  there. With more than four tiles they become compact (two columns, the icon left of the words), with more than six
  dense. HA stays in the left slot, Talk in the middle.
- **Conversations**: sessions separated by "Today · 14:02 · Home Assistant", the user's lines in bubbles on the right,
  the answers on the left with a gold rule, newest at the bottom, scrolling. Band: **Start over** (the next exchange
  starts a new session), Talk, **Clear** (asks first). The history (`crates/home-companion/src/history.rs`): text only,
  on the device (`<state_dir>/conversations.json`, mode 600), the last 7 days or 50 exchanges; Settings → "Keep
  conversations" off keeps nothing and deletes the file. Never audio.
- **Timers and alarms**: TIMERS rows (ring, name, "Ends at 14:33" or "Paused", countdown, pause/resume, cancel),
  ALARMS rows (time, days, switch, delete); a cancel or a delete hides the row and shows a 5 s chip with "Undo"
  before it really happens (no question). Band: **New timer** (a dialog: preset chips 1, 3, 5, 10, 15, 30 min and a
  stepper, gold "Start"), Talk, **New alarm** (hour and minute steppers, day chips, gold "Save"; no day = once).
  Timers set by touch are the body's own (`local-N`); pausing or cancelling one of Home Assistant's timers is local
  only (Wyoming cannot tell Home Assistant). Alarms are the body's (`crates/home-companion/src/alarms.rs`,
  `<state_dir>/alarms.json`) and ring like a timer.
- **Settings** (grouped rows, scrolling): the character's group ({name}'s memories, Language → its page, Keep
  conversations), DEVICE (Volume, Screen → night mode on/off, from, to; About, with the version; **Suspend {name}** →
  "Suspend {name}?" / "{name} won't listen or play until you press Vol+ or Vol–. A running timer wakes him by
  itself." Cancel · **Suspend**). Reached from More, or a double tap on the mouth.
- **Home Assistant** (the tile in More, the setup card, the control centre): linked, one row per check of the setup
  in Home Assistant (link, listening and voice, the brain's tools, the AI, the character's agent, speech and voice,
  who answers, the pause at the end of a sentence, the nightly tidy-up), each green, yellow or red with one line;
  band: "Set up" or "Check and repair", and Unlink. Not linked: the QR page below.
- **Music** (AirPlay; its tile, card and page show only while something plays): cover 304 px, source, title, artist,
  album; transport in the action band. Without a title from the sender: "Audio playing".
- **Screen mirroring** (AirPlay from a phone or a tablet, `page == "mirror"`, `crates/home-companion/src/ui/mirror.rs`):
  opens by itself when a session starts (over any page; Home Assistant's view and the control centre close) and
  closes back to that page when it ends. The sender's video centred on the character's background, as large as it
  fits without distortion (it re-lays out when the sender rotates). **Wide side bands** (a portrait sender: an
  upright phone leaves 456 px each side, a tablet 340): the left band is the character's stage (his pose 480 px
  tall, the idle phrase under it; in a conversation a small wave in the state colour, the phrase or the answer in two
  lines, and Stop), the right band the time (96 Light), the sender (`cast-connected`, gold, and its name), the
  sender's transport (previous · gold play/pause · next), the volume bar and **home** (`home`, the character's name).
  **A landscape sender** fills the screen: nothing on top at rest; a tap shows a 760 x 88 bar at the bottom for 4 s
  (home, the sender, previous · play/pause · next; a tap on the picture hides it); a conversation comes as Home
  Assistant's bar from the top. Home leaves the mirroring running: the home gets a card "Screen mirroring · <sender>
  · Tap to go back" that reopens it, with a close button that stops it. Before the first picture: the sender's name,
  a waiting line and the sliding line. A page left alone does not go home; the backlight stays up as over Home
  Assistant's view.
- **Link Home Assistant** (the QR page; at the first start on the device when no token is configured, until
  "Later"): a QR code (dark modules on ivory, whole pixels per module) with Home Assistant's authorisation page for
  this body ([design.md](design.md) section 8, `crates/home-companion/src/ha/link.rs`), three steps (scan, confirm
  in Home Assistant, done), a status line under them (gold when linked, or what went wrong in `danger`), the Wi-Fi
  and Home Assistant lines; band: the language and "Later" ("Close" once linked). Linked, the steps turn gold and the
  code dims; after the first setup the home shows a "Setup complete" card.
- **First start** (the device; the installer asks nothing): **Welcome** (`page == "welcome"`, when neither the
  configuration nor the settings have a language): the character on his stage, each locale's `first_welcome` side by
  side ("Welcome · Benvenuto"), one 112 px button per locale file named in itself ("English", "Italiano"); a tap
  applies it at once as Settings → Language does (the reflexes' model and the brain included) and goes on. Then
  **Where is Home Assistant?** (`page == "find-ha"`, when the configuration has no `[home_assistant] url` and none
  was picked): `avahi-browse -rpt _home-assistant._tcp` (`crates/home-companion/src/ha/discover.rs`) while a sliding
  line runs, then one row per instance (its name 28, its address 24 in `text-2`), or why nothing was found (not on
  the same network or zeroconf off; avahi-utils missing; the address by hand: the installer's `--ha-url` or the
  configuration). Band: the language · gold **Search again** · **Later**. A tap on a row saves it (`ha_url` in
  `settings.toml`; the configuration's address wins when set) and the body starts again (exit 5 for systemd's
  `Restart=`, or it executes itself) to wire Home Assistant; the QR page follows. Without an address, the Home
  Assistant tile and the HA button land here too. The name is the character's, never asked.
- **Start-up**: the loading animation on the stage, the character's name, "Starting…", a gold segment sliding on a
  hairline; no clock (the time may not be set yet).
- **Night** (Settings → Screen, 23-07 by default): only the clock, `night` grey, where it is by day, and one line under
  it (the next alarm within 12 hours, or a running timer); no cards, no actions, no gold. The character uses its
  `sleeping` pose, or its idle pose dimmed on the background when it has none, and stays still: with the night
  animations ([characters/README.md](../characters/README.md)) it falls asleep (`fall_asleep`), then breathes once a
  minute (`sleeping`, nothing redraws in between), and a touch or the morning plays `wake_from_sleep` before the
  home. A touch shows the normal home for the awake time; the backlight never goes above the night levels.
- **Screen off**: Vol+ with Vol− (outside Home Assistant's view) puts the backlight to 0 until a touch, a key, the
  wake word or a finished timer; a key press that only wakes the screen is not a volume step.
- **Volume**: a 360 x 56 HUD **top right**, next to the keys (on the ThinkSmart View, mounted at 270 degrees, they are
  on the top edge near the right corner): speaker glyph by level, gold fill, "40%"; 2 s after the last press. It never
  covers the action band or the stage.
- **Control centre**: pulled down from the top edge of the desk (never over the stage). One tile per thing that can be
  on or off, each with one short status line (`crates/home-companion/src/ui/control.rs`): Wi-Fi (it only tells:
  switching it off here would cut Home Assistant), Bluetooth (greyed while the device has no adapter), AirPlay,
  Microphone (off: no wake word, Talk still works), Night, Home Assistant; under them the volume and brightness bars
  (brightness "Auto" while it follows the light sensor).
- **Memories** (home-companion-brain; More, shown only when the brain answers, and Settings → "{name}'s memories"):
  the status line (memories, model, last nightly tidy-up), WHAT I KNOW ABOUT YOU (the profile block), NOTES, MEMORIES
  (facts, newest first, with their date). Each fact: the cross asks "Delete?" in the row itself, a second tap deletes.
  Action band: Talk, **Forget all** (a dialog: everything the brain knows goes, reminders stay).
- **Reminders**: a page of the reminders still to come; a due one is the home's message card (bell, gold, OK); OK =
  done (no voice after it); at night only the card (no wake-up of the screen). Late ones say so; missed ones are one
  card.
- **Shopping** (`[lists] shopping`, any to-do list of Home Assistant): a home card (cart, "Shopping list · 4 items", the
  first items; tap = the page), a tile in More and a page of open items (a circle and the text; a tap ticks it in
  Home Assistant and it goes).

## 7. Home Assistant's view

- At rest nothing is on top of the page. A tap anywhere (it still reaches the page) and the first 4 s after opening
  show the **home indicator** (160 x 6 ivory pill with a dark rim) and the **home button** (96 px, as big as Talk:
  `accent-deep` fill, gold ring, a white `home` glyph); they fade in in 150 ms and out in 300 ms, 4 s after the last
  touch (`[web] home_button_seconds`, 0 = always shown). The button takes a tap only after it has been shown for
  250 ms (a second quick tap on the page stays the page's).
- The button can be **dragged to any edge** (a touch that moves more than 12 px; it snaps 16 px in, 24 px above the
  bottom so it never covers the swipe strip); the place is kept in `<state_dir>/settings.toml`. The **indicator and
  the swipe follow it to its edge** (bottom by default; vertical on the left and right). Swiping in from that edge
  (a touch that starts in the 20 px strip) moves the page with the finger; armed, a gold chip with the character's
  face says "Release to go back to {name}".
- **Vol+ and Vol−** together go home at once, even with a hung page.
- **Heavy view**: a dialog in the middle (hourglass, "This view is heavy", "It takes about 10 seconds to open and uses
  a lot of memory.", Cancel · **Open** gold: the user just asked for it); asked once per view, then a 2 s note
  "Heavy view: it takes a while".
- **The page's `<select>`**: a 640 px sheet, group headers 20 Medium tracked, options 28 in 80 px rows, the chosen one
  on `surface-2` with a gold check, disabled ones in `text-3`, "Cancel"; a tap outside cancels.
- **"Okay Alfred" with the view open**: a bar over the top of the page (720 px, the character's face, the phrase or
  the answer in two lines at most, a small wave in the state colour, Stop). A longer answer, or a ringing timer,
  switches to the character's layout and the view comes back when it is over. While listening and thinking the
  page's frames are imported at most every 100 ms (the CPU is for the speech).
- **The view cannot open** (no helper, Home Assistant down): not a full-screen error but the character's layout with
  his error pose: "Home Assistant's view is not available", what happens next, band Close · **Retry**. The first
  load without a frame: the same layout with "Loading Home Assistant…" and the sliding line.

## 8. Navigation

```
                 ┌───────────── More ─────────────┐
 Home ──tap───▶  │ Conversations  Timers  Settings │ ──tap──▶ page ──back / 60 s alone──▶ home
  │  │           │ Home Assistant (Music, Memories…)│
  │  └─card──▶ the page of that card                └──────────────────────────────┘
  ├─pull from the top──▶ control centre
  ├─HA──▶ Home Assistant's view ──swipe in / home button / Vol+ Vol−──▶ Home
  │      (not linked yet: the HA button opens Link Home Assistant, the QR code)
  └─Talk or «Okay Alfred» (anywhere) ──▶ conversation ──▶ back where it started
 Timer or alarm finished ──▶ takes over everything ──Stop / tap──▶ back
 A phone mirrors its screen ──▶ Screen mirroring ──home──▶ Home (card ──tap──▶ back to it)
                                       └──the session ends──▶ the page it opened over
```

## 9. Motion and cost

Allowed: the wave (25-30 fps, 72 small rectangles), the toggle's knob (120 ms), Stop's ring colour (160 ms), the
HUD, the home button's fades (150 / 300 ms), the swipe (translate only), Home Assistant's view coming in and going
out (translate, 400 ms, a strongly decelerating curve, along the swipe of the home button's edge: it comes in the way
the swipe pushes it out, backwards; the character's own screen steps back under it, scaled to 94 % under a 35 % dark
veil, and is not drawn while the view covers it; after a swipe it goes on from where the finger left it), the
mirrored screen coming in and going out (the same 400 ms curve, up from the bottom edge, the home stepping back under
it, the video growing its last tenth), the "Okay Alfred" bar over it (down from the top, 180 ms), the timer ring
(1 Hz), the ringing ring (alpha, 1.6 s), the start-up line. Not allowed: blur, shadows, full-screen alpha animations
(a static scrim behind a dialog is fine), animated font sizes, anything moving on the idle home but the character.
The night pose is dimmed once in Rust (no opacity layer).

Build: the screen is compiled once for both renderers with fonts and icons embedded as files; the software renderer
(fallback, simulator, screenshots) rasterizes glyphs at run time from the embedded fonts, so there are no
pre-rendered glyph tables (with this type scale they would make the generated code and the release build huge; the
stripped binary on the device is about 17 MB).
