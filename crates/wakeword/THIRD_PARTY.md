<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Third-party code compiled into `wakeword`

`build.rs` compiles the following sources (fetched by `tools/fetch-tflm.sh` into the git-ignored
`third_party/`) into the crate, and so into every binary that uses it (`home-companion`, `wakeword-run`). Their licences
are compatible with the GPL-3.0-or-later of this project (Apache-2.0 is GPLv3-compatible; BSD-3-Clause is
permissive). Binary distributions must carry these notices; the packaging copies this file.

| Component | Version | Licence | Copyright |
|---|---|---|---|
| TensorFlow Lite Micro (core, reference kernels, flatbuffer schema) | github.com/tensorflow/tflite-micro commit `964e43d7ee3e82fe6abb6075b7f50506eafce79b` | Apache-2.0 | Copyright The TensorFlow Authors |
| microfrontend (`tensorflow/lite/experimental/microfrontend/lib`: frontend, window, filterbank, noise reduction, PCAN gain control, log scale, `fft.cc`, `fft_util.cc`) | github.com/tensorflow/tensorflow commit `e166dba74d0ff24174457522e3bf5655a117ab40` | Apache-2.0 | Copyright 2018 The TensorFlow Authors |
| FlatBuffers (headers only) | v25.9.23 | Apache-2.0 | Copyright Google Inc. |
| KISS FFT (`kiss_fft.c`, `kiss_fftr.c`, built with `FIXED_POINT=16`) | 131.2.0 | BSD-3-Clause | Copyright (c) 2003-2010 Mark Borgerding |

The microfrontend parameters and the streaming inference scheme follow ESPHome's `micro_wake_word` component
(github.com/esphome/esphome, `esphome/components/micro_wake_word/`); no ESPHome code is copied, the constants are
re-typed in `src/mww_shim.cc`.

Models are not code and carry their own licence next to them: `characters/alfred/wakeword/LICENSE` (MIT,
okay_alfred) and `fixtures/okay_nabu/LICENSE` (Apache-2.0, test fixture only).

## Apache License 2.0 notice (TensorFlow Lite Micro, microfrontend, FlatBuffers)

```
Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```

The full text of the licence is in `fixtures/okay_nabu/LICENSE` and at the URL above. None of these
components ships a NOTICE file at the pinned versions.

## BSD-3-Clause (KISS FFT)

```
Copyright (c) 2003-2010 Mark Borgerding . All rights reserved.

Redistribution and use in source and binary forms, with or without modification,
are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
this list of conditions and the following disclaimer in the documentation
and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its contributors
may be used to endorse or promote products derived from this software without
specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
