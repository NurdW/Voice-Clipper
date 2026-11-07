// SPDX-License-Identifier: GPL-3.0-or-later
//! Paths and configuration loading.
//!
//! Everything this tool needs lives in files it owns: `~/.config/clipper/` for
//! settings you edit by hand, `~/.local/state/clipper/` for the toggles the GUI
//! flips. Nothing here stores a secret, because nothing here needs one.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Where clipper keeps its own user-editable files.
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("could not find ~/.config")?;
    Ok(base.join("clipper"))
}

/// Volatile runtime state (the mute flag). Cleared on reboot is fine.
pub fn state_dir() -> Result<PathBuf> {
    // Propagate rather than expect: this runs inside the long-lived listener,
    // and a missing HOME should surface as an error message, not a panic.
    let base = match dirs::state_dir() {
        Some(base) => base,
        None => dirs::home_dir()
            .context("no state directory and no home directory to fall back on")?
            .join(".local/state"),
    };
    Ok(base.join("clipper"))
}

pub fn phrases_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("phrases.txt"))
}

pub fn muted_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("muted"))
}

/// Where GSR writes clips. Owns this decision outright: it used to be read out
/// of OBS's profile ini, which meant deleting OBS silently moved every clip to
/// a different folder. Set it by hand or with CLIPPER_GSR_DIR.
pub fn gsr_dir_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("gsr_dir.txt"))
}

/// Presence means "do not put the microphone on the recording".
pub fn gsr_no_mic_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("gsr_no_mic"))
}

/// Presence means "do not put desktop/game audio on the recording".
pub fn gsr_no_desktop_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("gsr_no_desktop"))
}

/// Presence of this file means "keep GPU Screen Recorder running".
///
/// Opt-in rather than the default: auto-starting a recorder nobody asked for is
/// a surprise, so the listener only does it once you flip the switch.
pub fn gsr_auto_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("gsr_auto_on"))
}

/// Holds the GSR replay-buffer length in seconds. The recorder has no config
/// file of its own, so the number chosen in the GUI lives here.
pub fn gsr_seconds_file() -> Result<PathBuf> {
    Ok(state_dir()?.join("gsr_seconds"))
}

/// Where the GSR per-source audio list lives.
pub fn gsr_audio_file() -> Result<PathBuf> {
    Ok(config_dir()?.join("gsr_audio.txt"))
}

/// Where the Vosk speech model lives (~/.local/share/clipper/...).
pub fn model_dir() -> Result<PathBuf> {
    let base = dirs::data_dir()
        .context("could not find ~/.local/share")?
        .join("clipper");
    Ok(base)
}

/// Find the downloaded Vosk model directory (any `vosk-model-*` under our share dir).
pub fn find_model() -> Result<PathBuf> {
    let base = model_dir()?;
    if !base.is_dir() {
        bail!(
            "no Vosk model in {}. Download one with:\n  mkdir -p {} && cd {} && \
             curl -LO https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip \
             && unzip vosk-model-small-en-us-0.15.zip",
            base.display(),
            base.display(),
            base.display()
        );
    }
    let mut found: Vec<PathBuf> = fs::read_dir(&base)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("vosk-model")))
        .collect();
    found.sort();
    found.into_iter().next().context("no vosk-model-* directory found")
}


/// Trigger phrases, one per line. Blank lines and `#` comments are ignored.
/// Vosk matches words case-insensitively against its own vocabulary, so we
/// lowercase everything here and keep the file predictable.
pub fn load_phrases() -> Result<Vec<String>> {
    let path = phrases_path()?;
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("could not read {} (see the example phrases file)", path.display()))?;
    let mut out: Vec<String> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let norm = t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
        if !out.contains(&norm) {
            out.push(norm);
        }
    }
    if out.is_empty() {
        bail!("{} has no phrases in it", path.display());
    }
    Ok(out)
}

/// Build the Vosk "restricted grammar" JSON: everything else becomes [unk].
/// This is what makes keyword spotting cheap enough to run all day.
/// Must be plain JSON — escaping the quotes here makes Vosk's FST parser fail.
pub fn build_grammar(phrases: &[String]) -> String {
    let mut parts: Vec<String> = phrases.iter().map(|p| format!("\"{p}\"")).collect();
    parts.push("\"[unk]\"".to_string());
    format!("[{}]", parts.join(", "))
}

pub fn ensure_dirs() -> Result<()> {
    for d in [config_dir()?, state_dir()?, model_dir()?] {
        if !Path::new(&d).exists() {
            fs::create_dir_all(&d).with_context(|| format!("could not create {}", d.display()))?;
        }
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::build_grammar;

    #[test]
    fn grammar_is_plain_json_with_unk_last() {
        let g = build_grammar(&["clip that".into(), "reddit".into()]);
        assert_eq!(g, "[\"clip that\", \"reddit\", \"[unk]\"]");
        // It must parse as JSON, or Vosk's FST reader rejects it outright.
        let v: serde_json::Value = serde_json::from_str(&g).expect("grammar must be valid JSON");
        assert_eq!(v.as_array().unwrap().len(), 3);
        assert_eq!(v.as_array().unwrap()[2], "[unk]");
    }
}
