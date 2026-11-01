# Goose In A Pond documentation

Every page here describes the repository as it is. Pages marked *record* are dated measurements,
audits or reports: their numbers are kept as measured, and a status line under the title says what
has changed since. When a page and the code disagree, the code is right; fix the page.

## Start here

```bash
bash scripts/giap.sh          # menu: install, build, service, logs, doctor
bash scripts/giap.sh doctor   # health check; exits 1 on any FAIL
```

| Page | For |
|---|---|
| [Quick start for contributors](QUICKSTART.md) | A first change, checked as CI checks it, in about ten minutes |
| [Installation](developer/installation.md) | What `giap.sh install` does, prerequisites, troubleshooting |
| [Contributing](CONTRIBUTING.md) | Branches, commits, comments, CI gates |
| [Testing](testing/tdd_guide.md) | The test layers and when each is required |
| [CLI reference](cli.md) | `pond-server` commands, `giap.sh` verbs, `make` targets |
| [REST API reference](api.md) | Every route, its auth and its DTOs |
| [Debug guide](developer/debug_guide.md) | Logs, data directory, common failures |
| [HOWTOAI.md](../HOWTOAI.md) | For coding agents: where things are and the traps |

## Architecture

| Page | For |
|---|---|
| [Overview](architecture/overview.md) | The hexagonal layout and the dependency rule |
| [Components](architecture/components.md) | Every crate and app, and whether it is on the live path |
| [Data flow](architecture/data_flow.md) | One chat turn from request to response and persistence |
| [Agent pipeline](architecture/agent_pipeline.md) | What runs around the model on each turn, and the SSE frames |
| [Visual workflow](architecture/visual_workflow.md) | A feature end to end, as diagrams |
| [Data pipeline](architecture/data_pipeline.md) | The March 2026 constrained-inference plan, with what landed |
| [Database schema](architecture/database_schema.md) | The three SQLite databases |
| [Settings catalogue](architecture/settings-catalogue.md) | Settings categories and the client contract |
| [Memory system](architecture/memory_system.md) | Batch extraction on the inference lane, scoring, decay, consolidation |
| [Scheduling](architecture/scheduling.md) | Routines, one-shots, event rules and the inference lane's batch jobs |
| [Model capabilities](architecture/model_capabilities.md) | Thinking, vision and context detection |
| [Token tracking](architecture/token_tracking.md) | Usage accounting per session |
| [MCP compliance](architecture/mcp-compliance.md) | What of the MCP spec GIAP implements |
| [Changes, 28 and 29 October 2026](architecture/changes-2026-10-28-29.md) | Long tasks, deep research, the queue, pace, the MTP drafter, My Pond and `agamemnon`, and their security controls |

## Personal Agentic Intelligence (PAI)

Work on any of PAI-1 to PAI-8 starts and ends with the checklist.

| Page | For |
|---|---|
| [PAI checklist](architecture/pai/00-checklist.md) | Status of every capability, and the next step |
| [Programme roadmap](architecture/personal-agentic-intelligence.md) | Why the eight capabilities, and how they depend on each other |
| [PAI-1 Identity and profiles](architecture/pai/01-identity-and-profile-boundaries.md) | |
| [PAI-2 Privacy and security](architecture/pai/02-privacy-and-security-guardrails.md) | |
| [PAI-3 Context governor](architecture/pai/03-context-governor.md) | |
| [PAI-4 Smart compaction](architecture/pai/04-smart-compaction.md) | |
| [PAI-5 Reasoning and thinking](architecture/pai/05-reasoning-and-thinking.md) | |
| [PAI-6 Multi-agent orchestration](architecture/pai/06-multi-agent-orchestration.md) | |
| [PAI-7 Proactive intelligence](architecture/pai/07-proactive-intelligence.md) | |
| [PAI-8 Personal context streaming](architecture/pai/08-personal-context-streaming.md) | |
| [Personal-context index](architecture/personal-context-index.md) | The local vector index behind PAI-8 |
| [Context space](architecture/context-space.md) | Designed: one item header over each kind's own table, one multimodal index and context agents (as recipes) in place of memories, summaries, the index and suggestions |
| [Context and reasoning roadmap](architecture/context-and-reasoning-roadmap.md) | Phases A to F, with status |

## Models and inference

| Page | For |
|---|---|
| [Models](developer/models.md) | Engines, sources, recommended picks, companions, acquisition |
| [Model architecture](developer/model_architecture.md) | Roles, providers and how a model is chosen |
| [Inference optimisation](developer/inference_optimization.md) | Agent backends, chat providers and engines, context sizing, MTP, and how to measure |
| [Prompt system](developer/prompt_system.md) | How the system prompt is built |
| [LiteRT-LM](developer/litert-lm.md) | The LiteRT-LM engine and its C API library |
| [mistral.rs direct backend](developer/mistralrs-direct-agent.md) | `--agent mistralrs`, off by default |
| [mistral.rs provider](developer/mistralrs-provider.md) | The Mac-only evaluation provider |
| [Running non-Gemma models](architecture/model-agnostic-slm-plan.md) | Plan: what breaks, and runtime detection |
| [Small-model tools](architecture/small-model-tools.md) | Tool design for small models; web search and knowledge |
| [Prefill cost audit](developer/prefill-cost-audit.md) | *Record* |
| [Realtime inference audit, 2026-09-24](developer/realtime-inference-audit-2026-09-24.md) | *Record* |

## Voice

| Page | For |
|---|---|
| [Voice mode](developer/voice-mode.md) | The voice stack, crate by crate |
| [Desktop voice child process](developer/voice-desktop-child-process.md) | `chat --voice --json-events` and the Electron side |
| [pond-ear](developer/pond-ear.md) | Wake word, Voice Match, endpointing, echo cancellation; built and measured, not yet wired into the voice loop |
| [Wake word calibration](wake-word-calibration.md) | Tuning the wake word |
| [TTS text normalisation](tts-text-normalization.md) | What happens to text before Kokoro speaks it |
| [Voice pipeline efficiency](voice-pipeline-efficiency.md) | Latency budget and what spends it |
| [Voice interrupt](developer/voice_pipeline_interrupt.md) | Barge-in during thinking and speech |
| [Voice dead-code inventory](developer/voice-dead-code-inventory.md) | What the old voice paths left behind, entry by entry |
| [Desktop voice screen](../pond-desktop/src/modes/VOICE_MODE.md) | The renderer side |

## Pairing, networking and remote access

| Page | For |
|---|---|
| [Auth and network posture](auth-network-posture.md) | Listeners, credentials, what each route accepts |
| [Networking](architecture/pond-networking.md) | Discovery, setup and claiming as one layer |
| [Setup channel](architecture/pond-setup-channel.md) | BLE discovery and first connection |
| [Connections and pairing settings](architecture/pond-connections.md) | |
| [Remote access](remote-access.md) | Reaching the pond away from home |
| [Embedded networking](../native/pondnet/README.md) | The Go tailnet node and mobile bindings |
| [Remote access deployment](../deploy/remote-access/README.md) | The coordinator side |
| [pondcredentials](architecture/pondcredentials.md) | Managed Apple Music credentials ([deploy](../deploy/pondcredentials/README.md)) |
| [Pinned HTTPS verification](pinned-https-verification.md) | Procedure |
| [Remote authorization verification](remote-authorization-verification.md) | Procedure |
| [Embedded remote access verification](embedded-remote-access-verification.md) | Procedure and ledger |

## Devices and services

| Page | For |
|---|---|
| [Matter](matter.md) | Controller lifecycle, commissioning, troubleshooting |
| [The giap-matter protocol](matter-protocol.md) | The WebSocket protocol to the matter.js controller |
| [Face recognition](face-recognition.md) | The `face-onnx` feature |
| [Music player](architecture/music-player.md) | Apple Music and Spotify ([extension](../extensions/music/README.md)) |
| [Rides and maps](architecture/rides-and-maps.md) | Uber on the household's own account |
| [Calendar and mail](guides/connect-calendar-and-mail.html) | Connecting CalDAV and IMAP |

## Extending GIAP

| Page | For |
|---|---|
| [Creating ports and adapters](developer/creating-ports-and-adapters.md) | Adding a capability without touching the core's purity |
| [Extensions](developer/extensions.md) | Building an MCP extension ([templates](../templates/extensions)) |
| [MCP server standard](developer/mcp-server-standard.md) | How GIAP's own MCP servers are written |
| [Weather, end to end](developer/example_workflow_weather.md) | One feature through every layer |
| [Goose integration](developer/goose_integrations.md) | What GIAP takes from Goose and how |
| [Goose patch management](goose-patch-management.md) | The fork, the patch set, syncing upstream |
| [Clean code and dependencies](developer/clean_code_and_dependencies.md) | |
| [Settings defaults and user intent](developer/settings-defaults-and-user-intent.md) | Changing a default without overriding a household's choice |

## Desktop

| Page | For |
|---|---|
| [Design rules](../DESIGN.md) | Precedence, the nine rules, the release gate |
| [Design system](design-system.md) | Tokens, primitives, class conventions |
| [Desktop server sidecar](developer/desktop-server-sidecar.md) | How the app stages and owns `pond-server` |
| [Onboarding](Onboarding_Doc/Onboarding_guide.md) | The first-run flow and its API gate |

## Jetson Orin Nano

| Page | For |
|---|---|
| [Dev routine](developer/jetson-dev-routine.md) | Build, deploy, service, logs and the traps; the emulator on the Mac |
| [Jetson scripts](../scripts/jetson/README.md) | The verbs `giap.sh` and `jetson.sh` delegate to |
| [Device tuning](developer/jetson-device-tuning.md) | *Record* |
| [Power-loss recovery](developer/jetson-power-loss-recovery.md) | Bringing the pond back after a power cut |
| [Engine bake-off](developer/jetson-engine-bakeoff.md) and [results](developer/jetson-engine-bakeoff-results.md) | *Record* |
| [In-process MTP scope](developer/jetson-mtp-in-process-scope.md) | Speculative decoding on the Orin; live on CUDA builds |
| [Re-prefill cost on the Orin](developer/orin-prefill-measurement.md) | *Record* |

## Records

| Page | |
|---|---|
| [Q1 progress report](reports/q1-progress-report.md) | *Record* |
| [Dual-engine benchmark, 2026-05-17](reports/dual-engine-benchmark-2026-05-17.md) | *Record* |
| [Security notes](security/) | Advisory remediations, dated |
| [Upstream: tsnet DNS fallback](upstream/tailscale-dnsfallback.md) | Draft upstream request |

## Licences

Code is [Apache 2.0](LICENSE); documentation and media are [CC BY 4.0](LICENSE-CC-BY-4.0).
