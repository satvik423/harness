# harness-context

The context engine (Phase 1 subset). Builds every model request and guards the
transcript invariants. Depends only on types and ports.

## Public API

| Item | Purpose |
|---|---|
| `SegmentContext` | Frozen system prompt and tools plus an append-only transcript |
| `SegmentSpec` | Inputs to open a segment (persona, tools, instructions, environment, number) |
| `ActivePersona`, `builtin_default()` | Persona settings; the built-in `default` uses Claude Opus 5.5 at high effort, 64k output |
| `load_project_instructions`, `find_git_root` | The `AGENTS.md` chain from the git root down to the working directory, capped at 32 KiB |
| `Environment` | The environment block in the segment opener (cwd, OS, date, git branch and status, session notes) |
| `BASE_PROMPT`, `BASE_PROMPT_VERSION`, `DEFAULT_SOUL` | System slots 1 and 2 |
| `civil_date` | UTC `YYYY-MM-DD` from Unix milliseconds |

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
