# ADR-044: JSON-schema structured output arrives in Phase 5

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 5 plan. Compaction needs a checkable JSON summary; both providers
  refused `ModelRequest::output_schema` with "arrives in Phase 15". The user chose to
  bring the schema part forward.

## Decision

- **OpenAI** (Responses API): `text.format = {type: "json_schema", name:
  "harness_output", schema, strict: true}`.
- **Anthropic** (Messages API, version `2026-10-21`): `output_config.format = {type:
  "json_schema", schema}`, next to `output_config.effort`. No beta header. Checked
  against Anthropic's current documentation.
- Both return the JSON as the reply's text. The runtime parses it and tolerates a
  Markdown fence around it.
- Schemas are written in the subset both strict modes accept:
  - every property required;
  - `additionalProperties: false`;
  - no numeric or length limits.

  A test walks the compaction schema to check this.
- Images and documents stay with Phases 11 and 15.

## Consequences

Compaction requests are constrained, so a well-formed summary comes back on the first
try. The Anthropic mapping is tested only against recorded request bodies: there is no
Anthropic key yet, so there is no live test.
