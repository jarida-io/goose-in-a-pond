#!/usr/bin/env python3
"""Split the converter's combined vision+audio mmproj into one file per encoder.

mtmd loads every tower in an mmproj, so measuring each encoder's memory needs its own file.
Usage: split_mmproj.py LLAMA_CPP_DIR IN.gguf OUT_PREFIX   -> OUT_PREFIX-vision.gguf, OUT_PREFIX-audio.gguf
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(sys.argv[1]) / "gguf-py"))
import gguf  # noqa: E402

DROP = {
    "vision": (("a.", "mm.a."), "clip.audio.", "clip.has_audio_encoder"),
    "audio": (("v.", "mm.input_projection"), "clip.vision.", "clip.has_vision_encoder"),
}


def split(reader, out_path, keep):
    tensor_prefixes, key_prefix, flag = DROP[keep]
    arch = reader.fields["general.architecture"]
    w = gguf.GGUFWriter(out_path, bytes(arch.parts[arch.data[0]]).decode())
    for field in reader.fields.values():
        name = field.name
        if name.startswith("GGUF.") or name == "general.architecture" or name.startswith(key_prefix):
            continue
        if name == flag:
            w.add_bool(name, False)
            continue
        vtype = field.types[0]
        sub = field.types[-1] if vtype == gguf.GGUFValueType.ARRAY else None
        w.add_key_value(name, field.contents(), vtype, sub_type=sub)
    kept = [t for t in reader.tensors if not t.name.startswith(tensor_prefixes)]
    for t in kept:
        w.add_tensor_info(t.name, t.data.shape, t.data.dtype, t.data.nbytes, t.tensor_type)
    w.write_header_to_file()
    w.write_kv_data_to_file()
    w.write_ti_data_to_file()
    for t in kept:
        w.write_tensor_data(t.data)
    w.close()
    print(f"{out_path}: {len(kept)} tensors, {sum(t.n_bytes for t in kept) / 1048576:.1f} MiB")


def main():
    reader = gguf.GGUFReader(sys.argv[2])
    for keep in ("vision", "audio"):
        split(reader, f"{sys.argv[3]}-{keep}.gguf", keep)


if __name__ == "__main__":
    main()
