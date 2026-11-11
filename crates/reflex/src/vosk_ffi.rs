// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Hand-written bindings to the prebuilt libvosk.so (vosk-api 0.3.45, vosk_api.h, Apache-2.0):
//! only the calls the reflex helper needs (crates/spike-native measured them in M0). Compiled into
//! the home-companion-reflex binary only (`--features vosk`), never into the library or the body.

use std::ffi::{CStr, CString, c_char, c_float, c_int};
use std::ptr::NonNull;

#[repr(C)]
struct VoskModel {
    _private: [u8; 0],
}

#[repr(C)]
struct VoskRecognizer {
    _private: [u8; 0],
}

#[link(name = "vosk")]
unsafe extern "C" {
    fn vosk_set_log_level(level: c_int);
    fn vosk_model_new(path: *const c_char) -> *mut VoskModel;
    fn vosk_model_free(model: *mut VoskModel);
    fn vosk_recognizer_new_grm(
        model: *mut VoskModel,
        rate: c_float,
        grammar: *const c_char,
    ) -> *mut VoskRecognizer;
    fn vosk_recognizer_set_words(rec: *mut VoskRecognizer, words: c_int);
    fn vosk_recognizer_accept_waveform(
        rec: *mut VoskRecognizer,
        data: *const c_char,
        len: c_int,
    ) -> c_int;
    fn vosk_recognizer_result(rec: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_partial_result(rec: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_final_result(rec: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_reset(rec: *mut VoskRecognizer);
    fn vosk_recognizer_free(rec: *mut VoskRecognizer);
}

pub fn set_log_level(level: i32) {
    unsafe { vosk_set_log_level(level) }
}

pub struct Model(NonNull<VoskModel>);

impl Model {
    pub fn new(path: &str) -> Result<Self, String> {
        let c = CString::new(path).map_err(|e| e.to_string())?;
        // SAFETY: valid C string; NULL on failure.
        NonNull::new(unsafe { vosk_model_new(c.as_ptr()) })
            .map(Model)
            .ok_or_else(|| format!("cannot load the Vosk model at {path}"))
    }
}

impl Drop for Model {
    fn drop(&mut self) {
        unsafe { vosk_model_free(self.0.as_ptr()) }
    }
}

pub struct Recognizer<'m> {
    rec: NonNull<VoskRecognizer>,
    _model: &'m Model,
}

impl<'m> Recognizer<'m> {
    /// A recognizer limited to `grammar_json` (a JSON list of phrases), with per-word results.
    pub fn with_grammar(model: &'m Model, rate: f32, grammar_json: &str) -> Result<Self, String> {
        let g = CString::new(grammar_json).map_err(|e| e.to_string())?;
        let rec = unsafe { vosk_recognizer_new_grm(model.0.as_ptr(), rate, g.as_ptr()) };
        let rec = NonNull::new(rec).ok_or("cannot create the Vosk recognizer")?;
        unsafe { vosk_recognizer_set_words(rec.as_ptr(), 1) };
        Ok(Self { rec, _model: model })
    }

    /// Feeds S16 samples; returns true when Vosk saw the end of an utterance.
    pub fn accept(&mut self, samples: &[i16]) -> bool {
        let bytes = std::mem::size_of_val(samples) as c_int;
        unsafe {
            vosk_recognizer_accept_waveform(self.rec.as_ptr(), samples.as_ptr().cast(), bytes) != 0
        }
    }

    /// The result JSON of the utterance that just ended (after `accept` returned true).
    pub fn result(&mut self) -> String {
        // SAFETY: Vosk returns a NUL-terminated string owned by the recognizer, valid until the next call.
        unsafe { CStr::from_ptr(vosk_recognizer_result(self.rec.as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }

    /// The partial result JSON of the utterance so far (`{"partial": "..."}`); does not end it.
    pub fn partial(&mut self) -> String {
        // SAFETY: Vosk returns a NUL-terminated string owned by the recognizer, valid until the next call.
        unsafe { CStr::from_ptr(vosk_recognizer_partial_result(self.rec.as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }

    /// Starts over (a new utterance).
    pub fn reset(&mut self) {
        unsafe { vosk_recognizer_reset(self.rec.as_ptr()) }
    }

    /// The final result JSON; flushes the decoder.
    pub fn final_result(&mut self) -> String {
        // SAFETY: Vosk returns a NUL-terminated string owned by the recognizer, valid until the next call.
        unsafe { CStr::from_ptr(vosk_recognizer_final_result(self.rec.as_ptr())) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for Recognizer<'_> {
    fn drop(&mut self) {
        unsafe { vosk_recognizer_free(self.rec.as_ptr()) }
    }
}
