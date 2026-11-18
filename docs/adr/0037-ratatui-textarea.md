# ADR-037: The input box uses `ratatui-textarea`

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Decision by the user at the start of Phase 4 (P4-T1)

## Context

Tech spec 3.1 names `tui-textarea` for the TUI's input box. Its last release (0.7,
October 2024) supports ratatui only up to 0.29; the current ratatui is 0.30. The ratatui
organisation now maintains the widget as `ratatui-textarea` (0.9.4, October 2026).

## Decision

Use `ratatui-textarea` (crossterm feature) with ratatui 0.30.2 and crossterm 0.29.

## Consequences

Current versions throughout; the API is the same family, so the spec's design holds.
