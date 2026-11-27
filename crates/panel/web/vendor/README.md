# Vendored for the board's editor

[gridstack.js](https://gridstackjs.com/) **14.1.0** (MIT, `LICENSE`), from `npm pack gridstack@14.1.0`
(published 2026-11-04), unchanged: the drag-and-resize grid of the panel's board editor (Board /
«Plancia»), with touch. Served by the panel as `/vendor/…` and kept a day like the fonts.

| File | From the package | sha256 |
|---|---|---|
| `gridstack-all.js` | `dist/gridstack-all.js` | `e12690e8e671d752c8007993967830e6a4b8bcfb2787ac85d1100dff765460b4` |
| `gridstack.min.css` | `dist/gridstack.min.css` | `74efc82159c5e8d28cb2cebd9f912be9d728bb07fa83896a6b62f4a843207126` |
| `LICENSE` | `LICENSE` | `28d28a6e0b5c6ebca8759b162a644c31cea28aab591060c0d95c7737d8456764` |

It places items through CSS custom properties set with the CSSOM (`style.setProperty`), which the
panel's Content-Security-Policy (`style-src 'self'`, no `'unsafe-inline'`) allows: no change to the
policy. To update: `npm pack gridstack@<version>`, copy the three files, write the new hashes here.
