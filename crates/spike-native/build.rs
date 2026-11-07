// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// Builds TensorFlow Lite Micro (core + reference kernels), the microfrontend and kissfft as static
// libraries, the C ABI shim (src/mww_shim.cc), and sets up linking against the prebuilt libvosk.so.
// The sources come from tools/fetch-tflm.sh and tools/fetch-vosk.sh (third_party/, git-ignored).
//
// The TFLM source list follows the pinned Makefile: MICROLITE_CC_BASE_SRCS (wildcards) and
// MICROLITE_CC_KERNEL_SRCS (read from tools/make/sources.inc), minus tests and test helpers.

use std::fs;
use std::path::{Path, PathBuf};

const FRONTEND: &str = "tensorflow/lite/experimental/microfrontend/lib";

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let tp = root.join("third_party");
    let tflm = tp.join("tflite-micro");
    let tf = tp.join("tensorflow");
    let flatbuffers = tp.join("deps/flatbuffers/include");
    let kissfft = tp.join("deps/kissfft");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let vosk = tp.join("vosk").join(&arch);

    for (path, script) in [
        (tflm.join("tensorflow/lite/micro/micro_interpreter.cc"), "fetch-tflm.sh"),
        (tf.join(FRONTEND).join("frontend.c"), "fetch-tflm.sh"),
        (flatbuffers.join("flatbuffers/flatbuffers.h"), "fetch-tflm.sh"),
        (kissfft.join("kiss_fftr.c"), "fetch-tflm.sh"),
        (vosk.join("libvosk.so"), "fetch-vosk.sh"),
    ] {
        if !path.exists() {
            panic!(
                "\n\n{} is missing.\nRun crates/spike-native/tools/{script} first \
                 (see crates/spike-native/README.md).\n\n",
                path.display()
            );
        }
    }
    for p in ["src/mww_shim.cc", "src/mww_shim.h", "third_party/tflite-micro", "third_party/tensorflow"] {
        println!("cargo:rerun-if-changed={p}");
    }

    // 1. TFLM + shim + the frontend's C++ files that wrap kissfft (C++17).
    let mut cxx = cc::Build::new();
    cxx.cpp(true)
        .std("c++17")
        .opt_level(2)
        .warnings(false)
        .include(&tflm)
        .include(&flatbuffers)
        .include(&kissfft)
        .include(&tf)
        .include(root.join("src"));
    for flag in [
        "-fno-exceptions",
        "-fno-rtti",
        "-fno-threadsafe-statics",
        "-fno-unwind-tables",
        "-fno-asynchronous-unwind-tables",
        "-ffunction-sections",
        "-fdata-sections",
    ] {
        cxx.flag(flag);
    }
    for def in [
        "TF_LITE_STATIC_MEMORY",
        "TF_LITE_DISABLE_X86_NEON",
        "TFLM_REMOVE_VIRTUAL_DELETE",
        "FLATBUFFERS_PREFER_PRINTF",
    ] {
        cxx.define(def, None);
    }
    cxx.define("FLATBUFFERS_LOCALE_INDEPENDENT", "0");
    cxx.files(tflm_sources(&tflm));
    cxx.file(root.join("src/mww_shim.cc"));
    cxx.compile("tflm");

    // 2. The microfrontend C library (only what the frontend needs: no *_io.c, *_main.c, memmap).
    let lib = tf.join(FRONTEND);
    let mut c = c_build(&tf, &kissfft);
    for name in [
        "frontend.c",
        "frontend_util.c",
        "window.c",
        "window_util.c",
        "filterbank.c",
        "filterbank_util.c",
        "noise_reduction.c",
        "noise_reduction_util.c",
        "pcan_gain_control.c",
        "pcan_gain_control_util.c",
        "log_scale.c",
        "log_scale_util.c",
        "log_lut.c",
    ] {
        c.file(lib.join(name));
    }
    c.compile("microfrontend");

    // 3. fft.cc / fft_util.cc (C++, used by frontend.c), then kissfft with FIXED_POINT=16 (C).
    //    Separate archives keep the GNU ld order linear: shim/tflm -> frontend -> fft -> kissfft.
    let mut fft = cc::Build::new();
    fft.cpp(true)
        .std("c++17")
        .opt_level(2)
        .warnings(false)
        .flag("-fno-exceptions")
        .flag("-fno-rtti")
        .include(&tf)
        .include(&kissfft)
        .file(lib.join("fft.cc"))
        .file(lib.join("fft_util.cc"))
        .compile("mfft");
    let mut kiss = c_build(&tf, &kissfft);
    kiss.define("FIXED_POINT", "16")
        .file(kissfft.join("kiss_fft.c"))
        .file(kissfft.join("kiss_fftr.c"))
        .compile("kissfft16");

    // 4. libvosk.so for vosk-grammar only: link search path here, `-lvosk` from #[link] in
    //    src/vosk_ffi.rs (compiled only into that binary), rpath $ORIGIN for the device copy.
    println!("cargo:rustc-link-search=native={}", vosk.display());
    println!("cargo:rustc-link-arg-bin=vosk-grammar=-Wl,-rpath,$ORIGIN");
}

fn c_build(tf: &Path, kissfft: &Path) -> cc::Build {
    let mut b = cc::Build::new();
    b.std("c11")
        .opt_level(2)
        .warnings(false)
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .include(tf)
        .include(kissfft);
    b
}

/// TFLM library sources, as the pinned Makefile selects them for a host build.
fn tflm_sources(tflm: &Path) -> Vec<PathBuf> {
    let micro = tflm.join("tensorflow/lite/micro");
    let mut files = Vec::new();
    for dir in ["", "arena_allocator", "memory_planner", "kernels/internal", "kernels/internal/reference"] {
        let d = micro.join(dir);
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "cc") {
                files.push(p);
            }
        }
    }
    let inc = fs::read_to_string(micro.join("tools/make/sources.inc")).expect("sources.inc");
    let mut in_kernels = false;
    for line in inc.lines() {
        if line.starts_with("MICROLITE_CC_KERNEL_SRCS :=") {
            in_kernels = true;
            continue;
        }
        if in_kernels {
            let token = line.trim().trim_end_matches('\\').trim();
            if token.is_empty() {
                break;
            }
            if let Some(rel) = token.strip_prefix("$(TENSORFLOW_ROOT)") {
                if rel.starts_with("tensorflow/lite/micro/kernels/") {
                    files.push(tflm.join(rel));
                }
            }
        }
    }
    // Tests, benchmarks and test helpers are not part of the library we need.
    files.retain(|p| {
        let name = p.file_name().unwrap().to_string_lossy();
        !(name.ends_with("_test.cc")
            || name.contains("benchmark")
            || matches!(
                name.as_ref(),
                "test_helpers.cc" | "test_helper_custom_ops.cc" | "fake_micro_context.cc" | "mock_micro_graph.cc"
            ))
    });
    files.sort();
    files.dedup();
    assert!(files.len() > 100, "unexpectedly few TFLM sources: {}", files.len());
    files
}
