// SPDX-License-Identifier: GPL-3.0-or-later
//! The mute flag.
//!
//! A file existing == muted. That is intentionally dumb: it means `clipctl
//! mute`, the GUI button, and a KDE keyboard shortcut can all flip the same
//! switch without any of them having to talk to the listener process.

use std::fs;

use anyhow::Result;

use crate::config::{self, muted_flag};

pub fn is_muted() -> Result<bool> {
    Ok(muted_flag()?.exists())
}

pub fn set_muted(muted: bool) -> Result<()> {
    let path = muted_flag()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if muted {
        fs::write(&path, b"muted\n")?;
    } else if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

pub fn toggle() -> Result<bool> {
    let now = !is_muted()?;
    set_muted(now)?;
    Ok(now)
}

/// Whether the listener should keep the replay buffer running even after you
/// switch it off by hand in OBS. Same one-file trick as the mute flag, so the
/// GUI can flip it without any IPC.
pub fn auto_buffer_enabled() -> Result<bool> {
    Ok(!config::auto_buffer_flag()?.exists())
}

pub fn set_auto_buffer(on: bool) -> Result<()> {
    let path = config::auto_buffer_flag()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if on {
        if path.exists() {
            fs::remove_file(&path)?;
        }
    } else {
        fs::write(&path, b"auto buffer disabled\n")?;
    }
    Ok(())
}

/// Whether the listener should keep GPU Screen Recorder running. Off unless
/// you turn it on — see `config::gsr_auto_flag` for why the polarity differs
/// from the OBS toggle.
pub fn gsr_auto_enabled() -> Result<bool> {
    Ok(config::gsr_auto_flag()?.exists())
}

pub fn set_gsr_auto(on: bool) -> Result<()> {
    let path = config::gsr_auto_flag()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if on {
        fs::write(&path, b"keep gsr running\n")?;
    } else if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// The clip length chosen for GPU Screen Recorder, if one ever has been.
/// `None` means never set, so the caller falls back to the default.
pub fn gsr_seconds() -> Option<u32> {
    let raw = fs::read_to_string(config::gsr_seconds_file().ok()?).ok()?;
    raw.trim().parse().ok()
}

pub fn set_gsr_seconds(secs: u32) -> Result<()> {
    let path = config::gsr_seconds_file()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, format!("{secs}\n"))?;
    Ok(())
}
