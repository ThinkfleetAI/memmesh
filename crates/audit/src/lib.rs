// Copyright 2026 ThinkFleet, Inc. Licensed under the Apache License, Version 2.0.

//! Access log + DLP pre-save guard hooks.
//!
//! Every read / write / subscribe routed through the server is logged via
//! this crate to the local `memory_audit_event` table (mirroring the
//! existing TS schema) and optionally streamed to a Shield endpoint when
//! configured. This is what makes the desktop a Shield foothold.
//!
//! Filled in by todo item: "Audit log + feedback loop + supersession".
