// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Compiles the companion screen in place (crates/spike-ui/ui/companion.slint; its font imports
//! resolve to crates/spike-ui/fixtures/fonts, its icons to crates/spike-ui/ui/design/icons) once,
//! with fonts and pictures embedded as files. Both renderers use the same code: femtovg draws
//! the fonts and rasterizes the SVG icons itself; the software renderer (fallback, simulator,
//! screenshots) registers the embedded fonts at start and rasterizes glyphs on demand
//! (Slint's `systemfonts`, on with `std`), so no glyphs are pre-rendered at build time: the
//! design's type scale used to make 148 MB of generated code and a release build above 8 GB.

use std::path::PathBuf;

use slint_build::{CompilerConfiguration, EmbedResourcesKind, compile_with_output_path};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let ui = manifest.join("../spike-ui/ui/companion.slint");
    let config = CompilerConfiguration::new().embed_resources(EmbedResourcesKind::EmbedFiles);
    let deps = compile_with_output_path(&ui, out.join("companion.rs"), config)
        .unwrap_or_else(|e| panic!("compiling {}: {e:?}", ui.display()));
    for d in deps {
        println!("cargo:rerun-if-changed={}", d.display());
    }
    println!("cargo:rerun-if-changed={}", ui.display());
    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("../spike-ui/fixtures/fonts").display()
    );
}
