// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// C ABI around TensorFlow Lite Micro and the microfrontend, shaped like ESPHome's micro_wake_word:
// audio in, one uint8 probability out per model invocation.

#ifndef HOME_COMPANION_WAKEWORD_MWW_SHIM_H_
#define HOME_COMPANION_WAKEWORD_MWW_SHIM_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define MWW_FEATURES 40

// Reasons recorded by mww_new when it returns NULL (read with mww_last_error).
#define MWW_OK 0
#define MWW_ERR_MODEL 1     // not a TFLite flatbuffer, wrong schema version, unexpected input/output
#define MWW_ERR_OPS 2       // op resolver or resource variables could not be set up
#define MWW_ERR_ARENA 3     // AllocateTensors failed: the tensor arena is too small
#define MWW_ERR_FRONTEND 4  // the microfrontend could not be initialised
#define MWW_ERR_MEMORY 5    // out of memory

typedef struct MwwHandle MwwHandle;

// Builds the interpreter and the audio front end. The model bytes are copied.
// step_ms is the manifest's feature_step_size. Returns NULL on error: the reason is then given by
// mww_last_error() and the TFLM log text by mww_last_message(), both on the calling thread.
MwwHandle* mww_new(const uint8_t* model, size_t len, size_t arena_bytes, int step_ms);

// Reason of the last mww_new failure on this thread (MWW_OK after a success).
int mww_last_error(void);

// TFLM's log output captured on this thread since the last mww_new (NUL-terminated, truncated).
const char* mww_last_message(void);

// Feeds up to n samples (16 kHz mono). Stops after the first new probability so the caller sees
// every one: *consumed tells how many samples were used. Returns 1 when *prob is new, 0 when all
// samples were consumed without a new probability, -1 on error.
int mww_process(MwwHandle* h, const int16_t* samples, size_t n, size_t* consumed, uint8_t* prob);

// Back to the state right after mww_new: front end, model input, variable tensors and resource
// variables (the streaming state). Returns 0 on success, -1 on error.
int mww_reset(MwwHandle* h);

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

#endif  // HOME_COMPANION_WAKEWORD_MWW_SHIM_H_
