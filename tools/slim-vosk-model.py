#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Slims a Vosk "small" model for the reflexes: they always decode a closed grammar, so the big
language model (graph/Gr.fst) is never used, but Vosk loads it anyway (~50 MB of RAM on the
ThinkSmart). This writes its word table to graph/words.txt (what Vosk reads without Gr.fst) and
moves Gr.fst to <model>/../<model-name>.Gr.fst (delete it when the reflexes work).

usage: slim-vosk-model.py MODEL_DIR
"""
import struct
import sys
from pathlib import Path

FST_MAGIC = 2125659606
SYMTAB_MAGIC = 2125658996


def words(path):
    """The output symbol table embedded in an OpenFst binary FST: [(word, id)]."""
    with open(path, "rb") as f:
        def i32():
            return struct.unpack("<i", f.read(4))[0]

        def i64():
            return struct.unpack("<q", f.read(8))[0]

        def string():
            return f.read(i32()).decode("utf-8")

        if i32() != FST_MAGIC:
            raise SystemExit(f"{path}: not an OpenFst file")
        string(), string()  # fst type, arc type
        i32()  # version
        flags = i32()
        f.read(8 + 8 * 3)  # properties, start, states, arcs

        def table():
            if i32() != SYMTAB_MAGIC:
                raise SystemExit(f"{path}: no symbol table")
            string()  # name
            i64()  # available key
            return [(string(), i64()) for _ in range(i64())]

        isyms = table() if flags & 1 else None
        osyms = table() if flags & 2 else None
        if not (osyms or isyms):
            raise SystemExit(f"{path}: no word table inside")
        return osyms or isyms


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        sys.exit(2)
    model = Path(sys.argv[1]).resolve()
    gr, out = model / "graph" / "Gr.fst", model / "graph" / "words.txt"
    if not gr.exists():
        print(f"{model}: no Gr.fst, nothing to do")
        return
    if not (model / "graph" / "HCLr.fst").exists():
        raise SystemExit(f"{model}: no HCLr.fst: not a model that decodes grammars without Gr.fst")
    table = words(gr)
    if not out.exists():
        out.write_text("".join(f"{w} {k}\n" for w, k in table), encoding="utf-8")
    kept = model.parent / f"{model.name}.Gr.fst"
    gr.rename(kept)
    print(f"{out}: {len(table)} words; Gr.fst moved to {kept}")


if __name__ == "__main__":
    main()
