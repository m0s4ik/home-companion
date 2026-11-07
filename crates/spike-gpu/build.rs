// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Compiles ui/app.slint twice:
//! - `app_gpu.rs`: the font embedded as a file, for femtovg (it cannot use
//!   glyphs pre-rendered for the software renderer);
//! - `app_sw.rs`: glyphs pre-rendered for the software renderer (the device
//!   image has no fontconfig fonts, so nothing may be looked up at run time).

use std::path::PathBuf;

use slint_build::{CompilerConfiguration, EmbedResourcesKind, compile_with_output_path};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let input = manifest.join("ui/app.slint");
    let builds = [
        ("app_gpu.rs", EmbedResourcesKind::EmbedFiles),
        ("app_sw.rs", EmbedResourcesKind::EmbedForSoftwareRenderer),
    ];
    for (name, kind) in builds {
        let config = CompilerConfiguration::new().embed_resources(kind);
        let deps = compile_with_output_path(&input, out.join(name), config)
            .unwrap_or_else(|e| panic!("compiling ui/app.slint for {name}: {e:?}"));
        for d in deps {
            println!("cargo:rerun-if-changed={}", d.display());
        }
    }
    println!("cargo:rerun-if-changed=ui/app.slint");
    println!("cargo:rerun-if-changed=fixtures/fonts");
}
