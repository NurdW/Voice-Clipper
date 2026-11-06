//! Paths and configuration loading.
//!
//! Deliberate rule for this whole project: the OBS websocket password is never
//! copied into our own config or into source code. We read it from OBS's own
//! config file at runtime, so rotating it in OBS's UI is enough — nothing here
//! goes stale and no secret is duplicated on disk.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Where clipper keeps its own user-editable files.
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::config_dir().context("could not find ~/.config")?;
    Ok(base.join("clipper"))
}

/// Volatile runtime state (the mute flag). Cleared on reboot is fine.
pub fn state_dir() -> Result<PathBuf> {
    let base = dirs::state_dir().unwrap_or_else(|| {
        dirs::home_dir()
            .expect("no home directory")
            .join(".local/state")
    });
    Ok(base.join("clipper"))
}

pub fn phrases_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("phrases.txt"))
}

pub fn muted_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("muted"))
}

/// Presence of this file means "do not keep restarting the replay buffer".
/// Absent means the listener keeps the buffer alive, which is what you want
/// while gaming and what the feature did before the toggle existed.
pub fn auto_buffer_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("auto_buffer_off"))
}

/// Presence of this file means "keep GPU Screen Recorder running".
///
/// Note the polarity is the opposite of `auto_buffer_off`: OBS is the original
/// recorder and was always kept alive, so its flag records the exception. GSR
/// is opt-in, because auto-starting a second recorder by default would double
/// the capture cost without anyone asking.
pub fn gsr_auto_flag() -> Result<PathBuf> {
    Ok(state_dir()?.join("gsr_auto_on"))
}

/// Holds the GSR replay-buffer length in seconds. OBS keeps its own length in
/// its own profile, but gsr has no config we own, so the number lives here.
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

#[derive(Debug, Default, Deserialize)]
pub struct ObsWsSettings {
    pub server_port: Option<u16>,
    pub server_password: Option<String>,
    pub auth_required: Option<bool>,
    pub server_enabled: Option<bool>,
}

impl ObsWsSettings {
    /// Read OBS's own obs-websocket config. Never printed, never logged.
    pub fn load() -> Result<Self> {
        let base = dirs::config_dir().context("no ~/.config")?.join("obs-studio");
        let path = base.join("plugin_config").join("obs-websocket").join("config.json");
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("could not read {} — is obs-websocket enabled in OBS?", path.display()))?;
        let s: Self = serde_json::from_str(&raw).context("obs-websocket config.json is not valid JSON")?;
        if s.server_enabled == Some(false) {
            bail!("obs-websocket server is disabled in OBS (Settings -> WebSocket Server). Enable it.");
        }
        Ok(s)
    }

    pub fn port(&self) -> u16 {
        self.server_port.unwrap_or(4455)
    }
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

/// Read a `key = value` pair out of ini-style text (OBS uses these).
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            current = t[1..t.len() - 1].to_string();
        } else if current == section
            && let Some((k, v)) = t.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// The folder OBS writes recordings/replays into, read straight out of its
/// profile ini files. Reading the files instead of asking OBS over the socket
/// means this works whether or not OBS is currently running.
///
/// OBS does not always record which profile is active, so: use the first
/// profile whose output folder actually exists, and if none exists yet, use the
/// first declared one. With several profiles and no running OBS this can pick
/// the wrong folder — set CLIPPER_GSR_DIR to override.
pub fn obs_record_dir() -> Option<PathBuf> {
    let base = dirs::config_dir()?.join("obs-studio");
    let profiles = base.join("basic").join("profiles");
    let mut found: Vec<PathBuf> = fs::read_dir(&profiles)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    let mut fallback = None;
    for dir in found {
        let Ok(ini) = fs::read_to_string(dir.join("basic.ini")) else {
            continue;
        };
        let Some(raw) = ini_value(&ini, "SimpleOutput", "FilePath")
            .or_else(|| ini_value(&ini, "Output", "FilePath"))
        else {
            continue;
        };
        let p = PathBuf::from(raw);
        if p.is_dir() {
            return Some(p);
        }
        if fallback.is_none() {
            fallback = Some(p);
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::build_grammar;
    use super::ini_value;

    #[test]
    fn grammar_is_plain_json_with_unk_last() {
        let g = build_grammar(&["clip that".into(), "reddit".into()]);
        assert_eq!(g, "[\"clip that\", \"reddit\", \"[unk]\"]");
        // It must parse as JSON, or Vosk's FST reader rejects it outright.
        let v: serde_json::Value = serde_json::from_str(&g).expect("grammar must be valid JSON");
        assert_eq!(v.as_array().unwrap().len(), 3);
        assert_eq!(v.as_array().unwrap()[2], "[unk]");
    }

    #[test]
    fn ini_lookup_finds_the_right_section_only() {
        let text = "[Basic]\nProfile=Untitled\n\n[SimpleOutput]\nFilePath=/mnt/storage/obs-record\nRecRBTime=300\n\n[Output]\nFilePath=/elsewhere\n";
        assert_eq!(ini_value(text, "Basic", "Profile").as_deref(), Some("Untitled"));
        assert_eq!(
            ini_value(text, "SimpleOutput", "FilePath").as_deref(),
            Some("/mnt/storage/obs-record")
        );
        assert_eq!(ini_value(text, "Output", "FilePath").as_deref(), Some("/elsewhere"));
        assert_eq!(ini_value(text, "SimpleOutput", "Nope"), None);
    }
}
