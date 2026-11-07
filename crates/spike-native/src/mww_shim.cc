// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera
//
// microWakeWord streaming inference with TensorFlow Lite Micro, following ESPHome's
// components/micro_wake_word (preprocessor_settings.h, streaming_model.cpp).

#include "mww_shim.h"

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <new>

#include "tensorflow/lite/experimental/microfrontend/lib/frontend.h"
#include "tensorflow/lite/experimental/microfrontend/lib/frontend_util.h"
#include "tensorflow/lite/micro/micro_allocator.h"
#include "tensorflow/lite/micro/micro_interpreter.h"
#include "tensorflow/lite/micro/micro_mutable_op_resolver.h"
#include "tensorflow/lite/micro/micro_resource_variable.h"
#include "tensorflow/lite/micro/schema/schema_generated.h"

namespace {

// ESPHome preprocessor_settings.h
constexpr int kSampleRate = 16000;
constexpr int kWindowMs = 30;
constexpr float kLowerBandHz = 125.0f;
constexpr float kUpperBandHz = 7500.0f;
constexpr int kNoiseSmoothingBits = 10;
constexpr float kNoiseEvenSmoothing = 0.025f;
constexpr float kNoiseOddSmoothing = 0.06f;
constexpr float kNoiseMinSignalRemaining = 0.05f;
constexpr float kPcanStrength = 0.95f;
constexpr float kPcanOffset = 80.0f;
constexpr int kPcanGainBits = 21;
constexpr int kLogScaleShift = 6;

// ESPHome streaming_model.h: resource variables live in their own small arena of 1024 bytes on
// the 32-bit ESP32. Its bookkeeping holds pointers, so it is scaled for 64-bit targets.
constexpr size_t kVariableArenaBytes = 1024 * sizeof(void*) / 4;
constexpr int kMaxResourceVariables = 20;
constexpr size_t kAlign = 16;

using OpResolver = tflite::MicroMutableOpResolver<20>;

bool register_ops(OpResolver& r) {
  // Exactly the ops ESPHome registers for streaming microWakeWord models.
  return r.AddCallOnce() == kTfLiteOk && r.AddVarHandle() == kTfLiteOk &&
         r.AddReshape() == kTfLiteOk && r.AddReadVariable() == kTfLiteOk &&
         r.AddStridedSlice() == kTfLiteOk && r.AddConcatenation() == kTfLiteOk &&
         r.AddAssignVariable() == kTfLiteOk && r.AddConv2D() == kTfLiteOk &&
         r.AddMul() == kTfLiteOk && r.AddAdd() == kTfLiteOk && r.AddMean() == kTfLiteOk &&
         r.AddFullyConnected() == kTfLiteOk && r.AddLogistic() == kTfLiteOk &&
         r.AddQuantize() == kTfLiteOk && r.AddDepthwiseConv2D() == kTfLiteOk &&
         r.AddAveragePool2D() == kTfLiteOk && r.AddMaxPool2D() == kTfLiteOk &&
         r.AddPad() == kTfLiteOk && r.AddPack() == kTfLiteOk && r.AddSplitV() == kTfLiteOk;
}

uint8_t* aligned_buffer(size_t bytes) {
  size_t rounded = (bytes + kAlign - 1) / kAlign * kAlign;
  return static_cast<uint8_t*>(std::aligned_alloc(kAlign, rounded));
}

}  // namespace

struct MwwHandle {
  uint8_t* model_data = nullptr;
  uint8_t* arena = nullptr;
  uint8_t* var_arena = nullptr;
  OpResolver resolver;
  tflite::MicroInterpreter* interpreter = nullptr;
  FrontendState frontend{};
  bool frontend_ready = false;
  int stride = 0;
  int stride_step = 0;
};

extern "C" int8_t mww_scale_feature(uint16_t value) {
  // ESPHome: value = round(feature * 256 / 666) - 128, where 666 ~ 25.6 * 26.0 (the float scale
  // used in training: input = feature * 256 / (25.6 * 26.0) - 128), clamped to int8.
  constexpr int32_t kScale = 256;
  constexpr int32_t kDiv = 666;
  int32_t v = (static_cast<int32_t>(value) * kScale + kDiv / 2) / kDiv - 128;
  if (v < -128) v = -128;
  if (v > 127) v = 127;
  return static_cast<int8_t>(v);
}

extern "C" void mww_free(MwwHandle* h) {
  if (h == nullptr) return;
  if (h->interpreter != nullptr) h->interpreter->~MicroInterpreter();
  std::free(h->interpreter);
  if (h->frontend_ready) FrontendFreeStateContents(&h->frontend);
  std::free(h->model_data);
  std::free(h->arena);
  std::free(h->var_arena);
  delete h;
}

extern "C" MwwHandle* mww_new(const uint8_t* model, size_t len, size_t arena_bytes, int step_ms) {
  auto* h = new (std::nothrow) MwwHandle();
  if (h == nullptr) return nullptr;
  h->model_data = aligned_buffer(len);
  h->arena = aligned_buffer(arena_bytes);
  h->var_arena = aligned_buffer(kVariableArenaBytes);
  if (h->model_data == nullptr || h->arena == nullptr || h->var_arena == nullptr) {
    std::fprintf(stderr, "mww: out of memory\n");
    mww_free(h);
    return nullptr;
  }
  std::memcpy(h->model_data, model, len);

  const tflite::Model* m = tflite::GetModel(h->model_data);
  if (m->version() != TFLITE_SCHEMA_VERSION) {
    std::fprintf(stderr, "mww: model schema %u, expected %d\n", static_cast<unsigned>(m->version()),
                 TFLITE_SCHEMA_VERSION);
    mww_free(h);
    return nullptr;
  }
  if (!register_ops(h->resolver)) {
    std::fprintf(stderr, "mww: cannot register ops\n");
    mww_free(h);
    return nullptr;
  }
  tflite::MicroAllocator* var_alloc =
      tflite::MicroAllocator::Create(h->var_arena, kVariableArenaBytes);
  tflite::MicroResourceVariables* vars =
      var_alloc ? tflite::MicroResourceVariables::Create(var_alloc, kMaxResourceVariables) : nullptr;
  if (vars == nullptr) {
    std::fprintf(stderr, "mww: cannot create resource variables\n");
    mww_free(h);
    return nullptr;
  }
  // Placement new: MicroInterpreter has no virtual delete in TFLM builds.
  void* mem = std::aligned_alloc(alignof(tflite::MicroInterpreter),
                                 (sizeof(tflite::MicroInterpreter) + kAlign - 1) / kAlign * kAlign);
  if (mem == nullptr) {
    mww_free(h);
    return nullptr;
  }
  h->interpreter = new (mem) tflite::MicroInterpreter(m, h->resolver, h->arena, arena_bytes, vars);
  if (h->interpreter->AllocateTensors() != kTfLiteOk) {
    std::fprintf(stderr, "mww: AllocateTensors failed (arena %zu bytes too small?)\n", arena_bytes);
    mww_free(h);
    return nullptr;
  }
  TfLiteTensor* in = h->interpreter->input(0);
  TfLiteTensor* out = h->interpreter->output(0);
  if (in->dims->size != 3 || in->dims->data[0] != 1 || in->dims->data[2] != MWW_FEATURES ||
      in->type != kTfLiteInt8 || out->type != kTfLiteUInt8) {
    std::fprintf(stderr, "mww: unexpected model input/output shape or type\n");
    mww_free(h);
    return nullptr;
  }
  h->stride = in->dims->data[1];

  FrontendConfig cfg;
  FrontendFillConfigWithDefaults(&cfg);
  cfg.window.size_ms = kWindowMs;
  cfg.window.step_size_ms = static_cast<size_t>(step_ms);
  cfg.filterbank.num_channels = MWW_FEATURES;
  cfg.filterbank.lower_band_limit = kLowerBandHz;
  cfg.filterbank.upper_band_limit = kUpperBandHz;
  cfg.noise_reduction.smoothing_bits = kNoiseSmoothingBits;
  cfg.noise_reduction.even_smoothing = kNoiseEvenSmoothing;
  cfg.noise_reduction.odd_smoothing = kNoiseOddSmoothing;
  cfg.noise_reduction.min_signal_remaining = kNoiseMinSignalRemaining;
  cfg.pcan_gain_control.enable_pcan = 1;
  cfg.pcan_gain_control.strength = kPcanStrength;
  cfg.pcan_gain_control.offset = kPcanOffset;
  cfg.pcan_gain_control.gain_bits = kPcanGainBits;
  cfg.log_scale.enable_log = 1;
  cfg.log_scale.scale_shift = kLogScaleShift;
  if (!FrontendPopulateState(&cfg, &h->frontend, kSampleRate)) {
    std::fprintf(stderr, "mww: FrontendPopulateState failed\n");
    mww_free(h);
    return nullptr;
  }
  h->frontend_ready = true;
  return h;
}

extern "C" int mww_process(MwwHandle* h, const int16_t* samples, size_t n, size_t* consumed,
                           uint8_t* prob, int8_t* features, uint16_t* raw) {
  *consumed = 0;
  while (*consumed < n) {
    size_t read = 0;
    FrontendOutput fo =
        FrontendProcessSamples(&h->frontend, samples + *consumed, n - *consumed, &read);
    *consumed += read;
    if (fo.size == 0) {
      if (read == 0) break;
      continue;
    }
    if (fo.size != MWW_FEATURES) return -1;

    // ESPHome perform_streaming_inference: write the slice at the current stride step,
    // invoke once the stride is full.
    TfLiteTensor* in = h->interpreter->input(0);
    h->stride_step %= h->stride;
    int8_t* dst = in->data.int8 + MWW_FEATURES * h->stride_step;
    for (int i = 0; i < MWW_FEATURES; ++i) dst[i] = mww_scale_feature(fo.values[i]);
    if (features != nullptr) std::memcpy(features, dst, MWW_FEATURES);
    if (raw != nullptr) std::memcpy(raw, fo.values, MWW_FEATURES * sizeof(uint16_t));
    ++h->stride_step;
    if (h->stride_step >= h->stride) {
      if (h->interpreter->Invoke() != kTfLiteOk) {
        std::fprintf(stderr, "mww: Invoke failed\n");
        return -1;
      }
      *prob = h->interpreter->output(0)->data.uint8[0];
      return 1;
    }
    if (features != nullptr) return 2;
  }
  return 0;
}

extern "C" size_t mww_arena_used(const MwwHandle* h) { return h->interpreter->arena_used_bytes(); }

extern "C" int mww_stride(const MwwHandle* h) { return h->stride; }
