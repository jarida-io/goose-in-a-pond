#!/usr/bin/env python3
"""Export the SpeechBrain spkrec-xvect-voxceleb model to ONNX.

Usage:
    pip install speechbrain onnx torch
    python scripts/export_xvector.py --output ~/.local/share/goose-in-a-pond/models/speaker.onnx

The exported model expects:
    input  "feats"     — float32 tensor [1, T, 40]   (log-mel filterbank frames)
    output "embedding" — float32 tensor [1, 512]     (x-vector speaker embedding)
"""

import argparse
import os
import sys
from pathlib import Path

def main():
    parser = argparse.ArgumentParser(description="Export SpeechBrain x-vector model to ONNX")
    parser.add_argument(
        "--output",
        type=Path,
        default=Path.home() / ".local" / "share" / "goose-in-a-pond" / "models" / "speaker.onnx",
        help="Output path for the ONNX file",
    )
    parser.add_argument(
        "--frames",
        type=int,
        default=200,
        help="Number of frames in the dummy input used for tracing (default: 200 = ~2 s)",
    )
    args = parser.parse_args()

    try:
        import torch
    except ImportError:
        print("Missing dependency: torch")
        print("Install with:  pip3 install speechbrain onnx torch")
        sys.exit(1)

    try:
        from speechbrain.inference.speaker import SpeakerRecognition
    except ImportError:
        try:
            from speechbrain.pretrained import SpeakerRecognition
        except ImportError as e:
            print(f"Missing dependency: {e}")
            print("Install with:  pip3 install speechbrain onnx torch")
            sys.exit(1)

    print("Downloading SpeechBrain spkrec-xvect-voxceleb model...")
    model = SpeakerRecognition.from_hparams(
        source="speechbrain/spkrec-xvect-voxceleb",
        savedir="/tmp/spkrec-xvect-voxceleb",
    )
    model.eval()

    encoder = model.mods["embedding_model"]
    encoder.eval()

    T = args.frames
    dummy = torch.zeros(1, T, 24)  # [batch=1, frames, n_mels=24]

    args.output.parent.mkdir(parents=True, exist_ok=True)

    print(f"Exporting to {args.output} ...")
    torch.onnx.export(
        encoder,
        dummy,
        str(args.output),
        input_names=["feats"],
        output_names=["embedding"],
        dynamic_axes={
            "feats":     {1: "time"},   # variable-length utterances
            "embedding": {0: "batch"},
        },
        opset_version=14,
    )

    # Quick sanity check
    try:
        import onnx
        m = onnx.load(str(args.output))
        onnx.checker.check_model(m)
        print("ONNX model check passed.")
    except ImportError:
        print("onnx package not installed — skipping model check.")

    print(f"\nModel saved to: {args.output}")
    print("You can now start the assistant with speaker identification:")
    print(f"  pond-server chat --input whisper --speaker-model {args.output}")
    print(f"\nEnroll your voice (run at least 3 times):")
    print(f"  pond-server enroll --profile <your-profile-id> --audio sample.wav")

if __name__ == "__main__":
    main()
