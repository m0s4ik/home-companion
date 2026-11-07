// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Compiles ui/app.slint with fonts and images pre-rendered for the software
//! renderer: the device image has no fontconfig fonts, so nothing may be
//! looked up at run time. ui/companion-m0.slint (the M1 companion screen, frozen; its landscape
//! screen, used only by the `companion-sim` binary) goes to its own file, so its fonts
//! do not end up in the spike binary.

fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedForSoftwareRenderer);
    slint_build::compile_with_config("ui/app.slint", config.clone()).expect("compiling ui/app.slint");
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("companion.rs");
    slint_build::compile_with_output_path("ui/companion-m0.slint", out, config).expect("compiling ui/companion-m0.slint");
    println!("cargo:rerun-if-changed=ui/companion-m0.slint");
    println!("cargo:rerun-if-changed=fixtures/fonts");
}
