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

- **Stage** x 0-424 (a third of the screen, `Grid.stage-width`): the character's own, nothing is drawn over it. A
  tap wakes the screen; a double tap on the mouth opens the settings.
- **Desk column** x 424-1232: 808 px, centre 828 (`Grid.col-x`, `col-width`, `col-cx`); 48 px from the right edge.
- **Bands** (y): page header 32-96 (back chevron + title, the time small at the right); home time band 48-290 (clock
  at 48, date line at 240); **middle band 312-600** (home: three 88 px cards, 12 px apart; pages: 112-608, their
  content **scrolls inside the band**, the header and the action band never move); **action band 640-776**: three
  160 x 136 slots centred at x 628, 828, 1028. The compact home (Settings → Screen) moves the clock up and the
  middle band to 228-616 (four cards), the action band to 656-792.
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
`Path` stays only for shapes that are data (the timer ring, the curves) and for the board's tiles, whose icons are
the user's choice among the whole set (§7): drawn from their path data at their size.

## 5. Components (`design/components.slint`)

`Icon`, `BareButton`, `PrimaryButton` (96 px disc; `stop` draws the square; Stop takes the state colour),
`PillButton` (64 px; gold only in dialogs), `RingButton`, `GlyphButton`, `Toggle`, `TimerRing` (empties clockwise from
12 o'clock, one short path a second), cards (`Card`, `InfoCard`, `TimerCard`, `NowPlayingCard`), `PageHeader`,
`Tile`, `SectionTitle`, `Group` + `Row` (chevron, check or switch), `Stepper`, `Chip`, `TimerLine` (24 px between
pause and cancel), `AlarmLine`, `ChatUser` / `ChatCharacter` (as tall as their text) / `ChatSeparator`, `Step`,
`Dialog` (scrim, icon or an icon's path data, title, text, content, Cancel ring + gold action), `VolumeHud`,
`ControlTile` and `LevelBar` (the control centre), `BoardTile` and `BoardGrid` (the board, §7). No shadows, no blur, no gradients over content (one 48 px fade at the top of the chat list).

The house **Yes/No question** (a cover, a valve) is a `Dialog` the brain owns: it opens on the brain's `confirm`
and closes on its `confirm_done` only (a tap marks it and waits; safety nets: its expiry, 3 s after a tap). A spoken
yes is taken back for one second: meanwhile the gold Yes button pulses (`confirm-blink`, about three pulses between
its ring and the gold fill) and No stays a target that wins.

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
- **Widget** (`crates/home-companion/src/ui/widgets.rs`, `WidgetPanel`): one panel under the cards that turns
  through what is around the house, up to eight pages in the user's order: **Coming up** (reminders and calendar
  events of today and tomorrow: the time in gold on the left, "tomorrow" with the time on the right), **Markets**
  (stocks, indices, currencies, coins chosen from the browser: name, the day's curve, value, change), **Weather**
  (no header: a big block, the sky 112 px on the left (144 on the compact home), a dotted text-3 divider, then
  "Now · Cloudy" in caption, the temperature in Light (96, 160 on the compact home) with high and low beside it,
  and a line with sunrise, sunset and the wind's arrow (where it blows to) with its speed; after today's sunset
  (`sun.sun`; 18:00 without it, or in polar day and night) it shows tomorrow: its sky, its high as the number,
  its low and wind. Under it, with three slots or more, the next hours in one small line (hour, sky,
  temperature; without an hourly forecast the next days with their high). Two slots: the block alone; one slot:
  sky, temperature, what it is, the range), **Other cities** (the time there in gold, the name, the sky, the temperature), **Shopping** ("4 to buy" in
  the header, one item a row), **Notes** (the board, a note a row), **Sensors** (Home Assistant's numbers chosen from
  the browser: name, the last 24 hours' curve, value, change against 24 hours ago), **To check** (doors and windows
  open, then batteries under 20 % from the emptiest; only when there is something), **Now playing** (a chosen media
  player, only while it plays; never Alfred's own AirPlay, which has its card) and **At home** (who is home, away or
  in a zone; off by default: anyone in the room reads it). Markets, Other cities and Sensors can be in the turn
  **more than once** (up to 4 each: "Markets", "Markets 2", or a name given in the browser), each page with its own
  list and its own place; the feeds fetch every page's items once. It comes in **two sizes only**, the same for every
  page (`widgets::size`): **full** when the cards leave 3 slots free or more (4 on the compact home with no card:
  still 3) = the three slots' height (288 px) whatever the page shows, its content from the top: a header (icon 28,
  the name in 20 Medium `text-3`, at the right a count, the rows shown or the time of old data) and up to 4 rows of
  52 px, the dots inside; **one line** with 1 or 2 free = one row as tall as a card without header (a page's own
  summary: the first quote without its curve, "1 open · 2 batteries", the first note and "+2"); 0 = no widget. Never a
  height in between, so the pages of the turn never change size. Rows that do not fit are cut, never
  shrunk; text never under 20 px, rows in 28. **More rows than fit** (Markets, Sensors, Notes, To check, Cities, At
  home): the page shows a window that moves on each time the page comes back ("5-8 of 8" in the header), or every
  interval when it is the only page. **A row with a number**: the curve (a `Path` whose commands Rust computes, 160 x
  32, stroke 2 px `text-2`, no fill), the value in a right-aligned column of 176 px (so the curves stand in a
  column), the change ("▲ 1.2%", "▼ 0.5"). **Colour only on the markets** (`ok` up, `danger` down): for a
  temperature or a power, up is neither good nor bad. **Old data** says when it is from ("data from 14:02") after
  two intervals and leaves the page after a day (the markets: two). **Cards and widget**: with its page on, the
  next reminder and the shopping list are pages of the widget, not cards; everything that asks for attention or a
  gesture stays a card and wins its slot. A page with nothing to say is not in the turn (no "No events today"); one
  page: no dots, no turn. The invitation shows only with neither a card nor a widget.
  **The turn**: every 20 s (Settings → Screen → Widgets: 20 s, 30 s, 1 min or by hand), driven by the body's
  one-second tick, never by an animation of the screen; it stands still while the home is not what is on screen
  (another page, Home Assistant's view, a mirrored screen, a conversation, a ring, a reminder full screen, the
  night, the screen off, the control centre, a dialog) and the page stays a whole interval when the home is back.
  The page on screen is a photograph: new data shows when it comes back, or at the minute (no thread ever writes a
  property: the feeds and Home Assistant's data wait in the screen until then). A swipe across turns it, a tap opens
  its own page (Reminders, Weather, Shopping, the trends, the city, Notes); To check, Now playing and At home
  have none (the widget's window already goes through every row) and do not even press. After either the turn waits a minute. Dots of 6 px, 16 apart, in the band's last 16
  px (the current one `accent`, the others `line`). Not at night.
  Settings → Screen → Widgets: the interval, then a switch per page in the turn's order (scrolls), with a line in
  `text-3` under the pages whose content is chosen from the browser ("3 chosen · Chosen from the browser"). The
  order, the markets, the cities, Home Assistant's entities are chosen only from the browser panel (no keyboard
  here); saved as `[widgets]` in settings.toml (`order`, `on`, `seen`, `quotes`, `cities`, `currency`, `sensors`,
  `player`, `people`, `check_openings`, `check_batteries`; a second page's list under its key, `[[widgets.quotes-2]]`,
  `sensors-2 = [...]`, its name in `names`).
- **The browser panel** (`crates/panel/web/`, not this screen, same tokens): four sections (Home, Screen,
  Shopping, Settings, with Access as the last group of Settings; a tab bar at the bottom on a phone, a rail on the
  left on a desk). Home draws the device's screen whole with the chosen widget in its slot, the rotation as a
  numbered strip under it and the chosen widget's editor beside it (on a desk one screen with no scrolling: the
  device as large as the height left by the strip, the strip's tiles one line each, only the editor scrolls); Screen is the remote screen (off unless switched
  on on the device, one controller at a time, the device's sign with Stop, private pages never sent). The widget's pages are drawn
  by the panel from the body's own page model (`widgets::panel_pages` → `panel/widget_ops.rs` `model`: the same
  words, numbers and cuts as here, with three free slots), in the screen's Inter subsets and with `WidgetPanel`'s
  sizes in `em` of 16 px at 808 px (`.wc` in panel.css): what changes there shows in its preview at once. A chosen
  item without data yet shows "…"; what to check and now playing show an example ("(example)"). Removing is a bin
  and an Undo for 7 s (the body keeps the last removal of each device for 15 s); only Revoke asks twice.
  The page fills the window (gutters grow with it; forms and lists keep 880 px). The turn is a miniature of
  this home screen in the device's frame (Alfred, the browser's clock, the widget where the screen puts it, the
  three keys), as wide as there is room and never past 1:1; the widgets in columns of at least 440 px. Alfred's
  portrait and poses (tools/make-art.py, ~110 KB of WebP and PNG) give the header, the pairing pages (the
  screen's access dialog drawn on the device) and a few empty states their face; the icon is his face.
- **The widget's pages** (opened by a tap): **trends** (the markets or the sensors: a card each, read only: name,
  symbol or "data from…", value 40 Tnum, change, the day's curve 520 x 96, high and low; the sources' credit under
  them), **city** (the weather page with another city's data in its own time zone: "local time 15:02" in the
  condition's line; the cities as chips above; "Weather data by Open-Meteo.com (CC BY 4.0)" under the forecast),
  **Notes** (a row per note with its date, ✕ asks "Delete?" and Undo stays 5 s; "To write one: by voice or from the
  browser"; the chips "Reminders · Notes" join it to the Reminders page, which has the same chips: More has no room
  for a ninth tile).
- **Offline** (Home Assistant not connected): the offline pose, the status card, HA dimmed to 35 %; Talk still works
  for the local reflexes.
- **Audio down** (`--state audio-down`, the audio did not start also after the reboot for it, `audio_down.rs`):
  home with a status card «The sound is not answering» in place of the offline one, for a minute before the next try.
- **Conversation**: the wave (72 bars, the state colour), the phrase 48, the heard sentence while thinking and small
  above the answer while speaking, the answer 34 up to five lines (the whole text is in Conversations). Stop takes
  Talk's place; in **error** ("I didn't understand", "Say it again? Just say «Okay Alfred»") the gold Talk stays, to
  try again; home after 6 s. A conversation is drawn over any page and leaves it as it was.
- **Timer or alarm finished**: takes over everything (Home Assistant's view too, which comes back afterwards):
  "Timer finished" (or "Alarm") in gold, the name 96 Light, the time since it ended counting up, "Touch anywhere to
  stop"; band "+1 minute" (a new local timer of one minute with the same name) and Stop, with a ring that breathes.
  The character uses `timer_finished`, falling back to `speaking`.
- **More**: tiles in the order of use: Timers and alarms, Reminders (when the brain answers), Music (while something
  plays), Shopping (when a list is configured), Memories (when the brain answers; "2 to confirm" when something
  waits), Conversations, Home Assistant, and Settings last in the bottom right corner; each says what is going on
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
- **Settings** (grouped rows, scrolling): the character's group (Language → its page, Keep
  conversations, "Answers to his name" with − and +: Less often · Normal · More often, a step around the wake word's
  cutoff — the configuration's `wake_word.cutoff`, else the model's — of +0.02 or −0.04, applied at once and kept in
  settings.toml; the log's near misses tell which one a voice needs; Conversation mode, a switch, off by default: after
  every spoken answer of Home Assistant (not a reflex's, nor with the microphone off) the microphone opens again
  without the wake word for `[conversation] follow_up_seconds`, and
  a window nobody speaks in closes quietly; "Times in a row", a row shown only while the switch is on, with − and +, 1 to 10, 3 at first: how many such windows
  (a question of his within them counts as one) before the wake word is needed again; `conversation_mode` and
  `conversation_max` in settings.toml, both on the browser panel too), DEVICE (Volume, Screen → night mode on/off, from, to; About, with the version; **Suspend {name}** →
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
- **Memories** (home-companion-brain; its tile in More, shown only when the brain answers: the only way in, since the
  page is used, not set): the status line (memories, model, last nightly tidy-up), TO CONFIRM (what Alfred heard once,
  such as a name: Right makes it known, Wrong drops it for good), WHAT I KNOW (what reaches the assistant in every
  conversation, newest first, with their date; the ones over the budget dimmed, found only when asked). Each fact: the
  cross asks "Delete?" in the row itself, a second tap deletes.
  Action band: Talk, **Forget all** (a dialog: everything the brain knows goes, reminders stay).
- **Reminders**: a page of the reminders still to come; a due one is the home's message card (bell, gold, OK); OK =
  done (no voice after it); at night only the card (no wake-up of the screen). Late ones say so; missed ones are one
  card.
- **Shopping** (`[lists] shopping`, any to-do list of Home Assistant): a home card (cart, "Shopping list · 4 items", the
  first items; tap = the page), a tile in More and a page of open items (a circle and the text; a tap ticks it in
  Home Assistant and it goes).

## 7. The board

Tiles of Home Assistant's entities (`crates/home-companion/src/ui/board.rs`, the model; `ui/board/draw.rs`, what
`BoardTile` draws; `ui/screen/board_ui.rs`, the screen's side), saved in `<state_dir>/board.json` and chosen from the
browser panel. Named "Board" ("Plancia" in Italian).

- **The page** (`page == "board"`) is **full screen**, as Home Assistant's view: the character's stage is not drawn
  under it (the home steps back as under that view, §8). The header: Alfred's face in a 56 px circle at x 24, y 20
  (the idle pose's crop around the mouth, `face`; a tap goes home), the page's name (34 Light, `text`) at x 100, the
  Wi-Fi and the time (28 Tnum, `text-2`) right-aligned at x 1255. A grid of **6 x 4 cells of 195 x 157 px, 12
  apart, at x 25, y 96** (1230 x 664, to y 760); the pages' dots at y 776 (6 px, 16 apart). Up to six pages of 24
  tiles; a page without tiles is not shown; pages laid out for the earlier 4 x 4 grid stay as they are. With nothing
  on the board yet: three rows of dashed places with a plus (`#24262d`, the plus `#33353d`) and, in the fourth,
  "Add tiles from the browser: Settings > Access from the browser." in `text-3`, on one line (keep it short in every
  language).
- **A conversation over it** ("Okay Alfred" with the board open): the face gets a 3 px ring in the state's colour
  (`state-color`), and the words come in the bar of Home Assistant's view (§8), here at x 96, y 8 beside the face
  and over the page's name, without a face of its own. An answer too long for the bar (over 70 characters,
  `answer-long`) takes Alfred's layout while he says it, and the board comes back afterwards; a ringing timer, a due
  reminder and the start-up do the same. The questions (house, tiles) and the done pop-up come over it, centred on
  the screen.
- **Following the finger**: from the home's right edge (a 40 px strip at x ≥ 1240, from y 48 so the control
  centre's pull keeps the top) the page comes in under the finger as it drags, the home stepping back under it;
  let go past a quarter of the screen (320 px) or with a flick (80 px in 250 ms) it goes on to its place, else it
  springs back (400 ms, Home Assistant's view's curve). Every touch there is logged as `edge_swipe` with its outcome.
  On the page, a drag across (12 px of slop, from a tile, a gap or the header) moves the pages with the finger, the
  neighbour coming in a screen's width beside it; let go by the same rule it slides on (260 ms) and the next page is
  drawn in place, else back; past the last page it gives a quarter of the drag. To the right on the first page the
  whole page goes out with the finger towards the home, by the same rule. Only x offsets move (the layer and the
  pages' row, whole pixels): no relayout of the tiles, nothing animated at rest. A drag never taps a tile; a drag
  along a level's bar sets the level. A thin handle (4 x 48 px, `#3a3832`, at x 1262, y 376) shows on the home only
  while a page has tiles. Back: the face, the home button or a swipe to the right on the first page; never by itself (like Home Assistant's view, it stays open). The home button is the view's: same look, same place (one saved place for both), shown 4 s after each touch, dragged to any edge.
- **A tile** is 1 x 1, 2 x 1, 2 x 2 or 4 x 1 cells (a level 2 x 1 or 4 x 1): `surface`, a 1 px `line` border,
  radius 24, padding 18 x 20 (14 x 18 in the widget's smaller cells). **Button**: the icon (40 px; 36 in the widget)
  at the top left, the name (24 Medium; 22) and the state (20; 18, "On · 60%" for a dimmed light, "Scene" for a
  scene) at the bottom; 2 x 2 centred with a 96 px icon. **Sensor**: 1 x 1 the icon 28 in `text-2`, the value 40
  Tnum, the name 20 `text-3`; wider a header (icon 28, the name 22 `text-2`), the value 48 Tnum at the bottom left and
  the day's curve (150 x 44 at 2 x 1, 440 x 44 at 4 x 1, stroke 2 px `text-2`); 2 x 2 the value 96 Light, the curve as
  wide as the tile, 84 tall, in `accent`, and "min 8.1° · max 14.9°" under it. **Level**: the name and its state on
  the top line, a 72 px bar (56 in the widget) on `surface-2` filled in `accent-deep`, the icon 32 in gold at its
  left end, the value 24 Tnum at its right end; a drag sets it, a tap on its icon's end switches it.
- **States**: off (`surface`, the icon `text-2`); **on** (`accent-deep`, a border of `accent` at 35 %, the icon and
  the state in gold; a level shows it with its bar); **waiting** after a tap until Home Assistant's new state (at
  most 4 s: the icon dims to 55 % once, in 180 ms, and stays so); **unavailable** (the icon `#4a4740`, the name
  `text-3`, no touch); **failed** (a `danger` border and "Didn't work" for 3 s, with the error sound); **stale**
  while the hub has lost Home Assistant (`HubMsg::Link`): the last state known, the whole tile at 45 %, a button's
  or a level's state "Not answering", no touch. Nothing moves
  at rest; a tile is redrawn only when its data changes, and only while it is seen (the page on screen, or the
  custom widget on the home, the screen on): what changed meanwhile is drawn when it comes back.
- **Icons** are MDI glyphs drawn from their path data (`Path`, 24 x 24 view box), the defaults by domain and device
  class built into the binary, any other read from the bundle's `icons/mdi.json` beside `locales/` (only the ones in
  use stay in memory).
- **A level** is sent when the finger lifts and at most every 300 ms while it drags. Let go, the bar stays where the
  finger left it (the tile waiting) until the entity's level is there or the 4 s are over, then shows the state; a
  touch taken away ends the drag where it was sent last.
- **The question**: a cover, a valve (or a tile with "Ask first") asks before its call, in the house question's
  window (§5): the tile's icon in gold instead of the question mark, "Close Shutter?" (the verb from what the call
  does to the state it finds: open/close, turn on/off, play/pause, run, "Set … to 40%?" for a level), the page's
  name under it, No · **Yes**, the 30 s line; 30 s without an answer is a No. The screen has **one** question: the
  brain's if it has one, else the tile's. A tap that would ask while the brain's question is open is refused (the
  error sound, nothing sent); a brain's question that comes while a tile's is open closes the tile's as a No.
  Neither is ever sent to nor answered by the remote screen. Locks, alarms, garage doors and gates are never on the
  board.
- **The custom widgets** (`widgets::Kind::Custom`): each of the board's widget canvases (`w1`…`w4`) is a page of
  the widget's turn of its own ("custom", "custom-2"…), with its place, its switch and its name (the canvas's) in
  Settings → Widgets and the browser's Home; added and removed with its canvas in the browser's Board; one without
  tiles has nothing to show. **4 x 2 cells of
  193 x 130** drawn in the slot itself (no panel around them; 272 px, the dots in the last 16). With one free slot:
  row 0 only, in one 88 px line, each tile its icon (32), name (20 Medium) and value or state (18). Its tiles take
  taps and drags as on the page; a swipe across still turns the widget.

- **The editor** is the browser panel's Board section (`crates/panel/web/panel.js`, ops in
  `panel/board_ops.rs`): the canvases on top, the grid at the device's proportions with the tiles the body draws
  (`board.get`: the same model as `BoardTile`; a page under its header, 6 columns as on the device) and the
  inspector beside it. On a desk it is one screen with no scrolling: the grid as large as the window lets it be (its
  width from the height left too, at the device's proportions), the inspector holding the chosen tile's form, the
  search when adding, or, with no tile chosen, the canvas itself (name, order, removing it); only the inspector
  scrolls, when its content is long (the icons, Advanced). On a phone the inspector goes under the grid. Drag and
  resize snap to the sizes a tile may have (gridstack.js, `crates/panel/web/vendor/`); without a pointer, the size
  buttons and Move. Every change saves the whole canvas; the body keeps what `board::validate` allows.

## 8. Home Assistant's view

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

## 9. Navigation

```
                 ┌───────────── More ─────────────┐
 Home ──tap───▶  │ Conversations  Timers  Settings │ ──tap──▶ page ──back / 60 s alone──▶ home
  │  │           │ Home Assistant (Music, Memories…)│
  │  └─card──▶ the page of that card                └──────────────────────────────┘
  ├─pull from the top──▶ control centre
  ├─swipe in from the right edge──▶ the board ──back / swipe right──▶ home (never alone)
  ├─HA──▶ Home Assistant's view ──swipe in / home button / Vol+ Vol−──▶ Home
  │      (not linked yet: the HA button opens Link Home Assistant, the QR code)
  └─Talk or «Okay Alfred» (anywhere) ──▶ conversation ──▶ back where it started
 Timer or alarm finished ──▶ takes over everything ──Stop / tap──▶ back
 A phone mirrors its screen ──▶ Screen mirroring ──home──▶ Home (card ──tap──▶ back to it)
                                       └──the session ends──▶ the page it opened over
```

## 10. Motion and cost

Allowed: the wave (25-30 fps, 72 small rectangles), the toggle's knob (120 ms), Stop's ring colour (160 ms), the
HUD, the home button's fades (150 / 300 ms), the swipe (translate only), Home Assistant's view coming in and going
out (translate, 400 ms, a strongly decelerating curve, along the swipe of the home button's edge: it comes in the way
the swipe pushes it out, backwards; the character's own screen steps back under it, scaled to 94 % under a 35 % dark
veil, and is not drawn while the view covers it; after a swipe it goes on from where the finger left it), the
mirrored screen coming in and going out (the same 400 ms curve, up from the bottom edge, the home stepping back under
it, the video growing its last tenth), the "Okay Alfred" bar over it (down from the top, 180 ms), the timer ring
(1 Hz), the ringing ring (alpha, 1.6 s), the start-up line, the widget's new page fading in (180 ms, once a
turn: about ten frames every 20 s; nothing moves between two turns), the board's page coming in (translate,
180 ms) and a tile's icon dimming once after a tap (180 ms). Not allowed: blur, shadows, full-screen alpha animations
(a static scrim behind a dialog is fine), animated font sizes, anything moving on the idle home but the character
(and the widget's fade, once a turn).
The night pose is dimmed once in Rust (no opacity layer).

Build: the screen is compiled once for both renderers with fonts and icons embedded as files; the software renderer
(fallback, simulator, screenshots) rasterizes glyphs at run time from the embedded fonts, so there are no
pre-rendered glyph tables (with this type scale they would make the generated code and the release build huge; the
stripped binary on the device is about 17 MB).
