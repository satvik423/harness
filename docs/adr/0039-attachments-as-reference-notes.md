# ADR-039: Attached files become reference notes until Phase 11

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Decision by the user at the start of Phase 4 (P4-T5)

## Context

The TUI turns pasted or dropped file paths into `InputItem::File`, but converting files
(images, PDFs, documents) is Phase 11 ingest work, and the runtime refused files.

## Decision

The runtime accepts `InputItem::File` and adds a short note to the user message:
`<attachment path="…" bytes="…" type="…">The user attached this file. Read it with
read_file if you need its contents.</attachment>`. The content is not inlined. A missing
file is an `attachment` error. Blob attachments stay unsupported until Phase 11.

## Consequences

The model reads attached files with its own tools (and the read-before-edit guard keeps
working). Phase 11 replaces the note with converted content and image blocks.
