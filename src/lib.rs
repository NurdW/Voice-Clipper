// SPDX-License-Identifier: GPL-3.0-or-later
//! clipper — voice-triggered OBS clipping, as a library so the CLI and the GUI
//! share exactly one implementation of "am I muted" and "talk to OBS".
//!
//! The binary is a thin front-end; the logic lives here,
//! so the GUI can never disagree with the CLI about what a toggle means.

pub mod config;
pub mod gsr;
pub mod listen;
pub mod mic;
pub mod obs;
pub mod sound;
pub mod state;
pub mod vosk;
