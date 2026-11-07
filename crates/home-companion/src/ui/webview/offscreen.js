// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// Pauses the endless animations of Home Assistant's frontend that are off the screen (the energy
// card's flowing dots below the fold kept WebKit drawing 60 frames a second, ~20% of a core, for a
// picture that did not change) and plays them again when they come into view. Shadow trees
// included. Found again every 2 s for the first half minute (cards keep arriving after the page
// is ready), then every 10 s (views change, cards come and go); checked every second.
(function () {
  if (window.__hcOffscreen) return;
  window.__hcOffscreen = true;
  let endless = [];
  const find = () => {
    const found = [];
    const walk = (root) => {
      const all = root.querySelectorAll('*');
      for (let i = 0; i < all.length; i++) {
        const e = all[i];
        if (e.getAnimations) {
          for (const a of e.getAnimations()) {
            if (a.effect && a.effect.getComputedTiming().iterations === Infinity) found.push([a, e]);
          }
        }
        if (e.shadowRoot) walk(e.shadowRoot);
      }
    };
    walk(document);
    endless = found;
  };
  const check = () => {
    const w = window.innerWidth, h = window.innerHeight;
    for (const [a, e] of endless) {
      const r = e.getBoundingClientRect();
      const seen = r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0 && r.top < h && r.left < w;
      if (seen && a.playState === 'paused' && a.__hcPaused) {
        a.__hcPaused = false;
        a.play();
      } else if (!seen && a.playState === 'running') {
        a.__hcPaused = true;
        a.pause();
      }
    }
  };
  let n = 0;
  find();
  check();
  setInterval(() => {
    if (++n % (n < 30 ? 2 : 10) === 0) find();
    check();
  }, 1000);
})();
