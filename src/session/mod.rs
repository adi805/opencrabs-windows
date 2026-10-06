//! Session surface for the Android client (PRD Feature 4).
//!
//! A loopback HTTP/JSON surface that mirrors the role Pi Durable's
//! `packages/client` + `packages/protocol` play: list sessions, read a
//! transcript, submit input idempotently, and subscribe to events.
//!
//! It exists because the native UI cannot drive the TUI, and A2A is
//! agent-to-agent task semantics rather than a transcript. See FR-006.

pub mod surface;
