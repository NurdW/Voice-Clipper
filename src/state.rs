// SPDX-License-Identifier: GPL-3.0-or-later
//! The mute flag and the recorder switches.
//!
//! A file existing == muted. That is intentionally dumb: it means `clipctl
//! mute`, the GUI button, and a KDE keyboard shortcut can all flip the same
//! switch without any of them having to talk to the listener process. Every
//! other toggle here uses the same trick for the same reason.

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

/// Whether the microphone goes on the recording. On unless you turn it off,
/// so the flag file records the exception and the default needs no file.
pub fn gsr_mic_enabled() -> Result<bool> {
    Ok(!config::gsr_no_mic_flag()?.exists())
}

pub fn set_gsr_mic(on: bool) -> Result<()> {
    let path = config::gsr_no_mic_flag()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if on {
        if path.exists() {
            fs::remove_file(&path)?;
        }
    } else {
        fs::write(&path, b"no mic\n")?;
    }
    Ok(())
}

/// Whether desktop and game audio goes on the recording.
pub fn gsr_desktop_enabled() -> Result<bool> {
    Ok(!config::gsr_no_desktop_flag()?.exists())
}

pub fn set_gsr_desktop(on: bool) -> Result<()> {
    let path = config::gsr_no_desktop_flag()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if on {
        if path.exists() {
            fs::remove_file(&path)?;
        }
    } else {
        fs::write(&path, b"no desktop audio\n")?;
    }
    Ok(())
}

/// Whether the listener should keep GPU Screen Recorder running. Off unless
/// you turn it on — see `config::gsr_auto_flag` for why it is opt-in.
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
