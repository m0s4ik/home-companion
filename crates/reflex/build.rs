// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// With `--features vosk`: link the helper against the prebuilt libvosk.so 0.3.45 (Apache-2.0),
// fetched by crates/spike-native/tools/fetch-vosk.sh into crates/spike-native/third_party/vosk/<arch>
// (git-ignored). On the device the library sits in /opt/home-companion/lib: rpath $ORIGIN/lib.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_FEATURE_VOSK").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let vosk = root.join("../spike-native/third_party/vosk").join(&arch);
    if !vosk.join("libvosk.so").exists() {
        panic!(
            "\n\n{}/libvosk.so is missing.\nRun crates/spike-native/tools/fetch-vosk.sh first.\n\n",
            vosk.display()
        );
    }
    println!("cargo:rustc-link-search=native={}", vosk.display());
    println!("cargo:rustc-link-arg-bin=home-companion-reflex=-Wl,-rpath,$ORIGIN/lib:$ORIGIN");
}
