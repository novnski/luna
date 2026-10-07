//! Forms: questions, MCP elicitation and provider pickers.
//!
//! OpenCode 2 asks through `form.created` (no top-level `sessionID`; route
//! by `data.form.sessionID`, or the `"global"` sentinel for MCP elicitation)
//! and is answered with `POST /api/session/{s}/form/{id}/reply {answer}` or
//! cancelled with `DELETE /api/session/{s}/form/{id}`. The 1.x
//! `question.asked` route and its `/question/*` endpoints were removed with
//! OpenCode 1 support (PD-15); the v2 form bridge onto LUNA's shared
//! question panel (OC-05) belongs in this module.
