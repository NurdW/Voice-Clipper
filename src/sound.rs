//! A short sound when a clip actually lands on disk.
//!
//! Deliberately fire-and-forget in a detached child process: a missing or
//! unreadable sound file must never stop a clip from being saved, so failures
//! are reported to the log rather than returned as errors.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::model_dir;

/// Where the clip sound lives. Install.sh puts it here.
pub fn sound_file() -> Result<PathBuf, anyhow::Error> {
    let dir = model_dir()?.join("sounds");
    for name in ["clip.wav", "clip.mp3", "clip.ogg", "clip.m4a"] {
        let p = dir.join(name);
        if p.exists() {
            return Ok(p);
        }
    }
    anyhow::bail!(
        "no clip sound in {} (expected clip.wav / .mp3 / .ogg)",
        dir.display()
    )
}

/// Play the clip sound. Returns Err only so the caller can log it; the clip
/// itself has already been saved by then.
pub fn play() -> anyhow::Result<()> {
    let path = sound_file()?;
    // pw-play is the native PipeWire player; paplay is the Pulse-compatible
    // fallback. Either is fine; both are on Fedora's pipewire-utils.
    let mut spawned = None;
    for player in ["pw-play", "paplay", "ffplay"] {
        let mut cmd = Command::new(player);
        if player == "ffplay" {
            cmd.args(["-hide_banner", "-loglevel", "error", "-nodisp", "-autoexit"]);
        }
        let result = cmd
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if result.is_ok() {
            spawned = Some(player);
            break;
        }
    }
    match spawned {
        Some(_player) => Ok(()),
        None => anyhow::bail!("no audio player found (tried pw-play, paplay, ffplay)"),
    }
}
