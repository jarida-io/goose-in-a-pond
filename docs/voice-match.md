# Voice Match

Voice Match lets the pond's room voice know which household member is speaking, so a spoken turn
gets that member's memories, particulars and notes. It gives context, never authority: a session
the pond knows only by voice never approves a draft, a proposal or a ride.

## Teaching it: one flow for the wake word and the voice

A member reads five sentences on the phone, each starting with the household's wake phrase ("Hey
goose, set a timer for ten minutes"). Each take is a `POST /api/v1/voice/takes` (multipart `audio`
as a 16 kHz mono WAV, `prompt`, `profile_id`, `voice_match`, default `true`). Serve:

1. transcribes the take with Whisper and records the phrase as heard, as `/voice/calibrate` does;
2. scores the take with the "hey goose" CNN (`TakeEar`, an ear of its own with no echo canceller);
3. with `voice_match` on, embeds the whole utterance, up to 12 s, with Voice Match's speaker model
   and stores the print in `voice_prints`;
4. discards the audio. Recordings are never kept.

The reply is `{take, of: 5, wake: {heard, heard_as, new_detector}, voice_match: {stored, enrolled,
consistency} | null, wake_detector: {kind, heard, of, reason, missed}}`. `new_detector` is whether
the CNN fired on this take; `wake_detector` is the household's verdict across every taught
member's latest run. Refusals are by name: 422 `no_speech`, 422 `no_wake_phrase` with `heard_as`,
422 `profile_required` (Voice Match on and nobody named), 409 `model_not_ready`.

A run is a member's takes until five, or until 15 minutes pass between two. A member is enrolled
from five prints; `consistency` is the mean cosine between their prints.

| Route | What it does |
|---|---|
| `GET /api/v1/voice/prints` | The model's state, progress and credit, and a row per member: takes, needed, enrolled, consistency, updated_at |
| `DELETE /api/v1/voice/prints/{profile_id}` | Forgets that member's voice; their face and wake calibration stay (204) |
| `POST /api/v1/voice/prints/model` | Fetches the speaker model again (202) |
| `DELETE /api/v1/faces/profile/{profile_id}` | Forgets the face only (`faces_deleted`) |
| `DELETE /api/v1/users/{id}/biometrics` | Forgets everything: face, voice prints and wake takes (`voice_prints_deleted`) |

## The wake detector

`voice_wake_detector = auto` uses the CNN only when the phrase is "hey goose" and the CNN heard at
least 4 of 5 takes of every taught member's latest run; otherwise Whisper on the ear's speech
segments, and once a member has three takes with a template, their own templates too (`personal`,
reason `taught`; `whisper` keeps them out). `reason` is one of `chosen`, `calibrated`,
`not_calibrated`, `custom_phrase`, `no_ear`, `taught`.

### Personal templates

A take with Voice Match on also keeps the member's wake template: Whisper's word times find the
phrase in the take, and the ear's own fbank frames of it are stored on the take, with the frames
after it (`voice_wake_takes.template`, `template_rest`). Features only, never audio. Forgetting the
voice (`DELETE /voice/prints/{id}`) clears them; forgetting the takes or the member removes them.

In the room each segment's opening is matched against every member's newest eight templates by
subsequence DTW in a Sakoe-Chiba band (`pond_ear::wake::personal`). A match needs a distance under
the member's threshold and a margin over the nearest of 36 embedded non-wake references ("he
goes", "a goose", "hey guys" and others, rendered with Kokoro). Each threshold is calibrated by
leave-one-out over the member's own takes, against their other words and the references. The rule
is Whisper's triggers or a template (`ear: wake by template`). Every segment is logged as
`kind="wake_shadow"` with Whisper's text, whether it matched the phrase strictly or by a learned
variant, and the template's distance, margin and thresholds, so the strict rule (the phrase alone,
or a template) can be judged on a household's own days before it is chosen.

On the offline bench (4 Kokoro voices, 5 takes each) templates found 38 of 40 wakes in a quiet room
with 1 false wake on 60 other sentences, half that under room reverb, and none in babble; the
strict rule matched Whisper's recall everywhere with no false wakes. Templates earn their place
only where Whisper hears a member's phrase as something else, which no synthetic voice shows.
Changing `voice_wake_word` through `PUT /settings` clears the learned pronunciations and the CNN's
results, because both belong to the phrase; a second member's run adds to them.

## In the room

The room's ear loads the speaker model once someone is enrolled, and gets the household's prints
at start and on every change (`RoomVoiceControl::household_changed`, called by the take and forget
routes). No cohort is built: Voice Match uses its default threshold with the single-profile
margin.

- A wake the ear recognises names its speaker (`WorkflowEvent::Wake { who }`, `last_heard` in
  `GET /voice/room`). The voice loop marks the session `identification_source = 'voice'` with
  `set_session_identity_if_stronger`, and the turn runs at that member's `Owner` scope with their
  `profile_context`.
- A different voice's wake starts a session of its own, as does an unrecognised voice after a
  member's turns, because `claim_session_identity` never moves a session to another member.
- A follow-up after a recognised wake must be the same voice; another voice's words are ignored
  without being transcribed. An unrecognised wake keeps today's follow-ups.
- With anyone enrolled, `interrupts` reads `enrolled_or_wake`: an enrolled voice over the reply
  for about 300 ms, or the wake word. With nobody enrolled, the wake word only.

## Identity and its limits

`IdentificationSource::Voice` ranks below `Face`. `identity_resolution::resolve` maps it to
`Owner` for context. `is_draft_decision_permitted` refuses an actor whose source is `Voice`, so
proposals are never decided on a voice match; rides are confirmed only from the member's paired
phone. Voice prints cascade with their profile, and the pruning pass sweeps orphans.

## The model

WeSpeaker's ResNet34-LM is fetched by default on serve's start to
`models/voice-match/resnet34_LM.onnx`, pinned to one revision and checksum, and converted once to
`.vmw` beside it; no load parses the ONNX after that. It must carry its credit, which
`GET /voice/prints` returns:

> Speaker embeddings by WeSpeaker ResNet34-LM (H. Wang et al., "WeSpeaker: A Research and
> Production oriented Speaker Embedding Learning Toolkit", ICASSP 2023), trained on VoxCeleb
> (A. Nagrani, J. S. Chung, A. Zisserman, "VoxCeleb: a large-scale speaker identification
> dataset", Interspeech 2017; J. S. Chung, A. Nagrani, A. Zisserman, "VoxCeleb2: Deep Speaker
> Recognition", Interspeech 2018). Licensed under CC BY 4.0,
> https://creativecommons.org/licenses/by/4.0/.

## Not verified

Everything here ran in Mac unit and route tests on 2026-11-02. No enrolment, recognised wake or
same-voice follow-up has run on the Orin, and the enrolment ear's memory with a 12 s span is
unmeasured.
