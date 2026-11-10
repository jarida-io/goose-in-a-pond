# Room voice bench

Measures the room's wake hits, false wakes and Whisper WER from real microphone recordings, by
replaying them through the same front end serve runs (`crates/pond-adapters-whisper/examples/ear_bench.rs`).

1. **Stimuli.** `sentences.tsv` is `id<TAB>text`, about 30 lines; ids starting `w` begin with the
   wake phrase, the rest do not (include sentences with the household's names and the learned
   variants' look-alikes). It holds household names, so it is not in the repository. Render each in
   several Kokoro voices to `tts/<voice>/<id>.wav`, and `babble.txt` the same way to `babble/`.
2. **Programs.** On the pond: `python3 build_program.py DIR GAIN_DB` writes `programs/` (quiet,
   babble at +10 and +3 dB, noise only) and `manifest.json`, every program between two sync chirps.
3. **Recording.** On the pond, with room voice off (`pond.py pair`, `pond.py room off`):
   `run_pass.sh A quiet babble10 babble3 noise silence`, and `at_base_volume.sh SOURCE B ...` for
   the same at the mic's card 0 dB. Put the room back (`pond.py room on`) and `pond.py revoke`.
4. **Scoring.** Anywhere with the recordings: `SILERO=... EAR_BENCH=... run_config.sh NAME MODEL A usb
   [--prompt TEXT] ...`, then `table.py NAME...`.

`score.py` reports per recording: wake hits on the `w` clips, false wakes on the others and on
noise and silence, and WER three ways: each clip's window transcribed alone (Whisper), the
segments as the ear cut them (the pipeline), and the request after a heard wake (the command).
