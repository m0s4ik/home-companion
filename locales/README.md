<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Locales

The body's own words (buttons, menus, dates), one file per language: `<code>.toml`, where `<code>` is the ISO 639-1
code (`en`, `it`, `de`…). What the character says is not here: it is in its `character.toml`
([characters/README.md](../characters/README.md)). English (`en.toml`) is the default and the reference: every
other file has exactly its keys, and a missing language falls back to English.

```toml
format = 1
name = "Italiano"            # the language named in itself, for the language menu
days = [...]                 # 7 names, from Sunday, as shown in the date (upper case)
months = [...]               # 12 names
[text]                       # the interface's words, same keys as en.toml
[sample]                     # sample data for the simulator, same keys as en.toml
```

Text uses only the characters of the screen fonts (characters/README.md, "Text"). `cargo test -p locale` checks every
file; a pull request adding a language must pass it.
