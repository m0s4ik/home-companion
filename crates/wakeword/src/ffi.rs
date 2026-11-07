// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Raw bindings to src/mww_shim.cc (built by build.rs). Only `detector.rs` uses them.

use std::ffi::CStr;

#[repr(C)]
pub struct MwwHandle {
    _private: [u8; 0],
}

pub const MWW_ERR_MODEL: i32 = 1;
pub const MWW_ERR_OPS: i32 = 2;
pub const MWW_ERR_ARENA: i32 = 3;
pub const MWW_ERR_FRONTEND: i32 = 4;
pub const MWW_ERR_MEMORY: i32 = 5;

unsafe extern "C" {
    pub fn mww_new(
        model: *const u8,
        len: usize,
        arena_bytes: usize,
        step_ms: i32,
    ) -> *mut MwwHandle;
    pub fn mww_last_error() -> i32;
    fn mww_last_message() -> *const std::ffi::c_char;
    pub fn mww_process(
        h: *mut MwwHandle,
        samples: *const i16,
        n: usize,
        consumed: *mut usize,
        prob: *mut u8,
    ) -> i32;
    pub fn mww_reset(h: *mut MwwHandle) -> i32;
    pub fn mww_arena_used(h: *const MwwHandle) -> usize;
    pub fn mww_stride(h: *const MwwHandle) -> i32;
    pub fn mww_free(h: *mut MwwHandle);
    fn mww_scale_feature(value: u16) -> i8;
}

/// TFLM's log text captured on this thread since the last `mww_new`.
pub fn last_message() -> String {
    // SAFETY: the shim returns a pointer to a NUL-terminated thread-local buffer.
    unsafe { CStr::from_ptr(mww_last_message()) }
        .to_string_lossy()
        .trim()
        .replace(['\r', '\n'], " ")
}

/// ESPHome's quantization of one front-end value to the model's int8 input, as compiled into the shim.
pub fn scale_feature(value: u16) -> i8 {
    // SAFETY: pure function.
    unsafe { mww_scale_feature(value) }
}
