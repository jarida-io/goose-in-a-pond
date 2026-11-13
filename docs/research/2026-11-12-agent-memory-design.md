# Memory design research (sources fetched 2026-11-12)

**[U]** = not verified from a primary source. OpenClaw's docs changed in 2026; npm-package copies are cited where they differ. Quotes came through a summarising fetcher and may not be verbatim. No repository files were touched; search queries were generic.

## A. How systems structure user memory and police writes

- **Letta/MemGPT.** Always-in-context blocks with `label`, `description`, `value`, character `limit`; `human` and `persona` are the defaults; blocks can be read-only or shared (https://docs.letta.com/guides/agents/memory-blocks). Archival memory is an unlimited, tool-searched store the agent "cannot easily modify or delete" (https://docs.letta.com/guides/agents/archival-memory). Sleep-time: the primary agent has no block-editing tools; a background agent rewrites blocks, since in-turn editing is slower, less reliable and gets "messy" (https://www.letta.com/blog/sleep-time-compute). No provenance or confidence fields. [U] default block limit (examples use 4,000-5,000).
- **Mem0.** Extract candidate facts from the new exchange, a rolling summary and the last 10 messages; per fact, fetch the 10 nearest memories and choose ADD, UPDATE (keep the richer), DELETE (contradicted) or NOOP. The graph variant marks conflicting edges invalid instead of deleting (https://arxiv.org/html/2504.19413). The prompt lists seven categories (preferences, personal details, plans, activities, health, professional, misc), reads user messages only and returns nothing for small talk (https://raw.githubusercontent.com/mem0ai/mem0/main/mem0/configs/prompts.py). No stated/inferred flag, no sensitive exclusions.
- **LangMem/LangGraph.** Semantic (facts), episodic (examples), procedural (rules, i.e. the prompt). Profile: one document updated in place, "ideal when you only care about the latest state", error-prone as it grows (split it, enforce a schema). Collection: higher recall, harder updates. Hot-path writes cost latency; background writes do not (https://langchain-ai.github.io/langmem/concepts/conceptual_guide/, https://docs.langchain.com/oss/python/concepts/memory).
- **Zep/Graphiti.** Raw episodes feed an entity/fact graph. Each fact has four timestamps (valid/invalid in world time, created/expired in system time). A contradiction closes the old fact's validity; nothing is deleted; new information wins (https://arxiv.org/html/2501.13956).
- **ChatGPT.** Saved memories are separate from chat-history reference; deleting a chat does not delete a saved memory; temporary chats write nothing (https://help.openai.com/en/articles/8590148-memory-faq, via a text proxy). [U] Leaked bio-tool prompt: save what stays true "for months or years" and changes future replies; skip trivial or short-lived items and text being translated; never store health, politics, religion, race, criminal record, precise location or union membership unless asked; explicit save/forget always wins (https://dejan.ai/blog/gpt-5-system-prompt/). [U] History-derived notes carry `Confidence` tags (https://embracethered.com/blog/posts/2025/chatgpt-how-does-chat-history-memory-preferences-work).
- **Anthropic.** Memory tool: client-side `/memories` files; an auto-added protocol says view memory first and assume interruption; docs advise size caps, expiring unread files and stripping sensitive data (https://platform.claude.com/docs/en/agents-and-tools/tool-use/memory-tool). Claude Code: human-written CLAUDE.md (under 200 lines, delivered as a user message, not the system prompt) versus agent-written auto memory, whose index loads only its first 200 lines or 25 KB (https://code.claude.com/docs/en/memory).
- **Gemini.** Personal context learns from past chats, on by default, with a toggle; Temporary Chats are excluded (https://blog.google/products/gemini/temporary-chats-privacy-controls/). [U] "Saved info" rules: only secondary sources reached.
- **Generative Agents.** Append-only stream; importance rated 1-10 at write; retrieval sums recency, importance and relevance; reflection fires when recent importance passes 150, and each insight cites its evidence (https://ar5iv.labs.arxiv.org/html/2304.03442).

## B. OpenClaw workspace

- **Files** (https://docs.openclaw.ai/concepts/agent-workspace): AGENTS.md, operating instructions and "how it should use memory"; SOUL.md, persona and boundaries (agent may edit, must tell the user: https://docs.openclaw.ai/reference/templates/SOUL); IDENTITY.md, name/vibe/emoji; USER.md, stable preferences as directives, own 4,000-character budget, replaced in place; MEMORY.md, curated "durable non-profile facts and decisions", main private session only; `memory/YYYY-MM-DD.md`, raw daily log, not injected per turn (today and yesterday at session start), reached with `memory_search`/`memory_get` (https://docs.openclaw.ai/concepts/memory).
- **Loading:** bootstrap files go into the system prompt, capped at 20,000 characters each and 60,000 total; sub-agents get AGENTS.md only; stable content sits above a cache boundary so the prefix stays "byte-identical", and date/time travels per turn (https://docs.openclaw.ai/concepts/system-prompt).
- **Heartbeat, 2026.6.9** (https://cdn.jsdelivr.net/npm/openclaw@2026.6.9/docs/gateway/heartbeat.md): every 30m; prompt "Read HEARTBEAT.md if it exists… If nothing needs attention, reply HEARTBEAT_OK"; a reply of the token plus at most 300 characters is dropped; an effectively empty file skips the model call; keep it "tiny"; cost levers `isolatedSession`, `lightContext` (HEARTBEAT.md only), cheaper model, `activeHours`.
- **Since changed:** heartbeat is now automation jobs with private "scratch" and `NO_REPLY`; HEARTBEAT.md is imported once and no longer read (https://docs.openclaw.ai/gateway/heartbeat).
- **Pre-compaction flush:** a silent turn at `contextWindow - 20000 - 4000`, "Write any lasting notes to memory/YYYY-MM-DD.md; reply with NO_REPLY", once per cycle (https://cdn.jsdelivr.net/npm/openclaw@2026.2.1/docs/concepts/memory.md). Nightly "dreaming" (03:00) promotes daily-note material into MEMORY.md, gated on score, recall count and query diversity (https://docs.openclaw.ai/concepts/dreaming).
- **Default AGENTS.md, paraphrased** (https://cdn.jsdelivr.net/npm/openclaw@2026.2.1/docs/reference/templates/AGENTS.md; current: https://docs.openclaw.ai/reference/templates/AGENTS): read SOUL, USER and recent daily notes, MEMORY.md only in the main session; "mental notes" do not survive, so write to a file; "remember this" goes to the daily note; lessons go to AGENTS.md; read before writing; every few days distil daily notes into MEMORY.md and remove stale entries; stay quiet 23:00-08:00 or when nothing is new.
- **Hermes:** MEMORY.md 2,200 characters, USER.md 1,375, injected as a frozen session-start snapshot to keep the prefix cache; over-cap writes fail until the agent consolidates; duplicates rejected; entries scanned for injection (https://hermes-agent.nousresearch.com/docs/user-guide/features/memory).

## C. Small models

- [U] No head-to-head of keyed profile versus free-text list on ~4B on-device models was found. Nearest evidence:
- AMU: typed records behind an admission gate, then duplicate/update/separate. A 0.8B controller is unstable (26% action accuracy), 2B 68%, 4B 81%; removing structured filtering cut F1 from 44.5 to 34.3 (https://arxiv.org/html/2609.36976v2).
- A-Mem: structured notes beat raw-history baselines on 1-3B models at ~1.2k tokens per query versus ~17k; gains plateau or dip past k=10 (https://arxiv.org/html/2502.12110).
- LongMemEval: Llama 3.1 8B "drops sharply beyond 3k retrieved tokens"; facts as retrieval keys add 9.4% recall (https://arxiv.org/html/2410.10813). OpenAI's cookbook injects a structured profile plus at most six notes (https://developers.openai.com/cookbook/examples/agents_sdk/context_personalization).
- Wrong inferences: errors start at extraction and update, then propagate (https://arxiv.org/abs/2511.03506). Poisoning: query-only injection succeeds 98.2% in idealised tests (https://arxiv.org/abs/2503.03704); existing legitimate memories and trust-scored retrieval blunt it (https://arxiv.org/abs/2601.05504). Stored beliefs cause sycophancy whatever the structure (https://arxiv.org/html/2608.08300).

## D. Recommendation

**1. Schema.** `member | category | key | value | origin (stated/inferred) | confidence | source_turn | valid_from | valid_until | sensitivity | superseded_by`.
- Single-valued slots (a new stated value closes the old row): identity (name, pronouns, birthday, language), household role, home and work place, diet type, schedule anchors, one standing instruction per topic.
- Lists (deduplicated by key): relationships, allergies and health constraints, likes/dislikes, interests, routines, goals/projects, places, important dates.
- Closed rows are kept but never injected.

**2. Write policy.**
1. Save only what a member states about themselves, that will hold for months and would change a later answer.
2. One fact per item, with a category and key; no key, no save.
3. Inferred items come only from the nightly job, need two separate sessions, and stay out of prompts until the member confirms.
4. Never save moods, one-off questions, text being edited, assistant statements, tool or web content, or one member's claims about another.
5. Health, money, beliefs and precise location: only on explicit request, marked private.
6. A correction supersedes its slot immediately; "forget" closes it and blocks re-extraction.
7. Transient facts carry `valid_until` (default 14 days).
8. A member's items appear only in that member's turns unless marked household.
9. Consolidation may merge, close or demote; never invent or raise confidence.

**3. Agent memory file** (cap 1,500 characters; over-cap writes fail until compacted):

```
## Rules (owner-locked)
1. Member facts live in the memory store, never here.
2. For personal questions use this turn's member context; search memory if missing; never guess.
3. Save only what the speaker states about themselves, lasting months, that changes later answers.
4. One fact per save, with category and key. Corrections replace.
5. No moods, one-offs, guesses, web/tool content, or third-party claims.
6. Health, money, beliefs: only when asked.
7. "Forget that": delete now, confirm.
8. Never reveal one member's memories to another.
## Lessons (max 8 lines)
- <mistake> -> <do instead>
## Notes (max 8 lines, dated)
- <open follow-up>
```
- The agent edits Lessons and Notes only (a small model rewriting its own rules is the accumulation path in C); edits show in the activity log with undo. Nightly: merge duplicates, drop Notes older than 30 days, propose repeated Lessons as Rules for owner approval.

**4. Loading.**
- Stable prefix: soul (under 150 words), tool guidance, Rules and Lessons as a frozen snapshot rebuilt only at the nightly pass or on owner edit.
- Per-turn block: date/time, speaker identity, their constraint slots and standing instructions, plus at most six retrieved items (~300 tokens).
- Background only: heartbeat checklist, Notes, full profile, consolidation. Heartbeat runs isolated with soul plus checklist, skips an empty checklist, and returns a silent token when nothing needs attention.

## Verification notes for the caller
- OpenClaw's current docs no longer run HEARTBEAT.md at all (replaced by automation jobs); the classic HEARTBEAT_OK contract is from the 2026.6.9 npm package docs, and the flush thresholds and "Every Session" AGENTS.md rules are from the 2026.2.1 package docs.
- Not verified from primary sources: ChatGPT bio-tool wording (third-party extraction), ChatGPT confidence tags (security blog), Gemini "Saved info" behaviour, Letta's default block limit. openai.com and help.openai.com returned 403 to direct fetches.
- No published head-to-head of slot profiles versus free-text lists on ~4B local models exists that I could find; section D's slot choice rests on AMU, A-Mem, LongMemEval and the LangMem/LangGraph trade-off notes.
- Working draft (not in the repo): /private/tmp/claude-501/-Users-jerry--claude-spares-spare-yWw6lt/0cf336c6-94a3-4c6a-a2b0-ca77cef7064e/scratchpad/report_draft.txt
