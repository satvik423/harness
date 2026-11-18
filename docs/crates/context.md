# harness-context

The context engine. It builds every model request, guards the transcript invariants,
and (Phase 5) decides when to compact and builds the compaction: rendering, request,
validation and the next segment's opener. It depends only on types, protocol and
ports; everything here is pure, and the runtime does the I/O.

## Public API

| Item | Purpose |
|---|---|
| `SegmentContext` | Frozen system prompt and tools plus an append-only transcript; `replay_user` rebuilds from stored messages, `push_note` queues a system note for the next user message, `ends_with_reply` |
| `SegmentFrame` | Everything frozen when a segment opens (provider, model, system, tools, limits, opener); stored as a blob so a resumed segment sends the same prefix (ADR-032) |
| `SegmentSpec` | Inputs to open a segment (persona, tools, instructions, environment, number) |
| `ActivePersona`, `builtin_default()` | Persona settings; the built-in `default` uses Claude Opus 5.5 at high effort, 64k output |
| `load_project_instructions`, `find_git_root` | The `AGENTS.md` chain from the git root down to the working directory, capped at 32 KiB |
| `Environment` | The environment block in the segment opener (cwd, OS, date, git branch and status, session notes) |
| `BASE_PROMPT`, `BASE_PROMPT_VERSION`, `DEFAULT_SOUL` | System slots 1 and 2 |
| `civil_date` | UTC `YYYY-MM-DD` from Unix milliseconds |
| `Budget`, `Pressure` | The compaction threshold, `min(ratio × window, compact_at_tokens)`, and how full a request is (`Near` within 10%: count exactly) |
| `SegmentContext::record_input`, `estimated_input` | The last measured input plus an estimate of what was appended since (P5-T1) |
| `SegmentFrame::successor` | The next segment's frame: same prefix, new number and opener |
| `compaction::render` | A segment's events as text with `#seq` on every line; tool results in their capped form; the opener's environment block left out |
| `compaction::compaction_request`, `output_schema`, `parse_output` | The request (instructions, focus, active pins, structured-output schema) and the reply's parsing |
| `compaction::validate` | I5 (pins must quote their event exactly), I6 (anchors clipped, overlaps cut, gaps filled with `unlabelled` anchors at step boundaries), pin merging and supersession |
| `compaction::render_opener`, `CompactionStyle` | The next segment's opener: environment, summary (fixed template), active pins verbatim, the anchor index; `SummaryOnly` is the eval baseline |
| `cap_output`, `estimate_tokens` | The capped form of a tool result, decided once when recorded: head 60% + tail 40% of the cap + `[output trimmed: N tokens total · full: recall {"seq_from":S,"seq_to":S}]` (FR-TOOL-04) |

## Request layout

1. System: base prompt (slot 1), persona soul (slot 2), project instructions (slot 5).
2. Tools, sorted by name, frozen for the segment.
3. First user message: the environment opener as block 0 (cache breakpoint), then the input.
4. The conversation, append-only.

## Transcript rules (`push_*` methods)

| Method | Allowed when | Refuses |
|---|---|---|
| `push_user` | start; after an assistant reply without tool calls; after a user message whose turn produced no reply (refusal, empty) | input containing tool calls/results; input while tool calls are unanswered |
| `push_assistant` | the last message is from the user | empty messages; non-assistant roles |
| `push_tool_results` | the last assistant message has tool calls | anything but exactly one result per pending call (I2) |

Nothing can be edited or removed, so every request extends the previous one (I1).

## Compaction (Phase 5)

`render` → `compaction_request` → (the model, structured output) → `parse_output` →
`validate` → `render_opener`. The validated `CompactionRecord` (types in
`harness-protocol`, ADR-045) holds every pin so far and the segment's anchors. Pins keep
session-wide ids (`p3`) and anchors continue across segments (`a7`). Property tests
check that any set of proposed anchors ends up covering the segment exactly once.
