# Keynote: Democratizing AI with Goose In A Pond

A 15-minute talk for the MCP conference.

| File | What it is |
|---|---|
| [architecture.html](architecture.html) | The architecture diagram, drawn with Jarida Ink. Open it in a browser. |
| [architecture.png](architecture.png) | The same diagram at 3200 px wide, for slides and docs. |
| [../../site/](../../site/) | The landing page for pond.jarida.io, with the showcase screenshots and the slides as a PDF. |

The slides are on [pond.jarida.io](https://pond.jarida.io/#keynote) as a PDF.

## Where the numbers come from

Every figure on the slides is in the code or the repo's docs:

| Figure | Source |
|---|---|
| 17 MCP servers, 74 tools | `crates/pond-adapters-goose/src/giap_registration.rs`, `#[tool]` functions in `crates/pond-mcp-server/src` |
| 7,620 MB budget on the Orin Nano 8 GB | `device_profile.rs`, `device_budget.rs` |
| About 1 s to the first word, 15-16 tokens a second (Gemma 4 E4B) | `crates/pond-core/src/models/domain/recommended.rs` |
| 2.1-2.3x decode with the MTP drafter, token-identical | `docs/developer/inference_optimization.md`, Speculative decoding |
| Wake-word CNN: 75,777 parameters, 304 KB, 72% validation recall, 3 of 16 household clips | `crates/pond-ear/models/hey-goose.md` |
| Fact sheet: at most 100 lines, rebuilt daily or after 5 new memories | `domain/fact_sheet.rs`, `services/fact_sheet.rs` |
| Zero ungated HTTP calls | `crates/pond-core/tests/egress_guard.rs` (`MAX_UNGATED = 0`) |
| Five authors | `Cargo.toml` `authors` |
