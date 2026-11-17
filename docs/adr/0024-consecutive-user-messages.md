# ADR-024: A user message may follow an unanswered user message

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

After a refusal or an empty reply the turn produces no assistant message, so the user's message is the last one. A strict "no two user messages in a row" rule would then block all further input.

## Decision

`SegmentContext::push_user` accepts input after a user message whose turn produced no reply. Both providers merge consecutive user turns. Tool pairing (I2) and the no-prefill rule are unchanged.

## Consequences

A refusal never leaves a session stuck.
