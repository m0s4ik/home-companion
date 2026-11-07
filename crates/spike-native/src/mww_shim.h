// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// C ABI around TensorFlow Lite Micro and the microfrontend, shaped like ESPHome's micro_wake_word:
// audio in, one uint8 probability out per model invocation.

#ifndef SPIKE_NATIVE_MWW_SHIM_H_
#define SPIKE_NATIVE_MWW_SHIM_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MWW_FEATURES 40

typedef struct MwwHandle MwwHandle;

// Builds the interpreter and the audio front end. The model bytes are copied.
// step_ms is the manifest's feature_step_size. Returns NULL on error (reason on stderr).
MwwHandle* mww_new(const uint8_t* model, size_t len, size_t arena_bytes, int step_ms);

// Feeds up to n samples (16 kHz mono). Stops after the first new probability so the caller sees
// every one: *consumed tells how many samples were used. Returns 1 when *prob is new, 0 when more
// audio is needed, -1 on error. When features is not NULL (debugging), it also stops after every
// feature slice: the quantized slice (MWW_FEATURES int8) is copied to features, the raw uint16
// front-end values to raw, and 2 is returned when no new probability came with the slice.
int mww_process(MwwHandle* h, const int16_t* samples, size_t n, size_t* consumed, uint8_t* prob,
                int8_t* features, uint16_t* raw);

// Bytes of the tensor arena actually used after AllocateTensors.
size_t mww_arena_used(const MwwHandle* h);

// Model input stride (feature slices per invocation).
int mww_stride(const MwwHandle* h);

void mww_free(MwwHandle* h);

// ESPHome's quantization of one front-end value to the model's int8 input.
int8_t mww_scale_feature(uint16_t value);

#ifdef __cplusplus
}  // extern "C"
#endif

#endif  // SPIKE_NATIVE_MWW_SHIM_H_
