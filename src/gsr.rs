// SPDX-License-Identifier: GPL-3.0-or-later
//! Control for GPU Screen Recorder (`gpu-screen-recorder`), the recorder this
//! tool drives.
//!
//! gsr is driven entirely by signals — `SIGUSR1` saves the replay buffer,
//! `SIGINT` stops recording — so this module needs no socket client and no
//! extra dependencies. Process discovery scans `/proc` for the same reason.
//!
//! Settings come from the environment (CLIPPER_GSR_*) when set, otherwise from
//! the state and config files this crate owns.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};

use crate::config::gsr_dir_file;

pub const BIN: &str = "gpu-screen-recorder";

fn var(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Sources gsr can actually see right now, as `name|description` lines.
fn known_audio_devices() -> Vec<String> {
    let out = Command::new(BIN)
        .arg("--list-audio-devices")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    out.lines()
        .filter_map(|l| l.split('|').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Drop sources gsr would reject, so one absent device cannot stop the recorder
/// from starting at all.
///
/// This is what broke clipping mid-game: the list names `easyeffects_source`,
/// EasyEffects was not running, and gsr exits with "not a valid audio device"
/// rather than skipping it — so the listener restarted a dying process every
/// 30 seconds and nothing ever recorded. `app:` names are exempt because gsr
/// tolerates an app that is not running (it makes a silent track); a missing
/// *device* is fatal.
fn filter_available(sources: Vec<String>) -> Vec<String> {
    let known = known_audio_devices();
    // Empty means the probe itself failed; better to pass the list through
    // unchanged than to silently record nothing.
    if known.is_empty() {
        return sources;
    }
    let mut kept: Vec<String> = Vec::new();
    for s in sources {
        let ok = s.starts_with("app:")
            || s == "default_output"
            || s == "default_input"
            || known.iter().any(|k| k == &s);
        if ok {
            kept.push(s);
        } else {
            eprintln!("clipper: skipping audio source '{s}' (not present right now)");
        }
    }
    // Never lose the microphone: if everything mic-shaped was dropped, fall back
    // to the raw default input.
    let has_mic = kept
        .iter()
        .any(|s| s.contains("input") || s.contains("easyeffects") || s.contains("mic"));
    if !has_mic {
        eprintln!("clipper: no microphone source available, using default_input");
        kept.insert(0, "default_input".to_string());
    }
    kept
}

/// Parse a `gsr_audio.txt` body: one source per line, `#` starts a comment.
/// Kept separate from `audio_list` so the rules are testable without touching
/// the filesystem.
fn parse_audio_list(txt: &str) -> Vec<String> {
    txt.lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Is this source the microphone (as opposed to game and app sound)?
///
/// `easyeffects_source` is a virtual device with no "input" in its name, so it
/// has to be named; everything else that captures follows PipeWire's
/// `alsa_input.*` / `default_input` naming.
pub fn is_mic_source(source: &str) -> bool {
    source == "easyeffects_source" || source.contains("input")
}

/// Drop the sources the user switched off. Split out from `apply_audio_flags`
/// so it can be tested without touching the real flag files.
fn filter_audio(list: Vec<String>, mic: bool, desktop: bool) -> Vec<String> {
    list.into_iter()
        .filter(|s| if is_mic_source(s) { mic } else { desktop })
        .collect()
}

/// Both flags default to on, so with no flag files this returns the list
/// untouched.
pub fn apply_audio_flags(list: Vec<String>) -> Vec<String> {
    filter_audio(
        list,
        crate::state::gsr_mic_enabled().unwrap_or(true),
        crate::state::gsr_desktop_enabled().unwrap_or(true),
    )
}

/// Audio sources, one `-a` each, so every source lands on its own track.
///
/// gsr joins sources written with `|` into ONE mixed track, but a separate `-a`
/// per source produces a separate stream tagged with where it came from
/// (`name=Applications: discord`), which is what lets an editor set the level
/// of each one independently. Verified with ffprobe on a real clip rather than
/// read off the man page, which does not mention it.
///
/// The list is read from `~/.config/clipper/gsr_audio.txt`, one source per
/// line, `#` for comments. `CLIPPER_GSR_AUDIO` still wins for a one-off.
pub fn audio_list() -> Vec<String> {
    let list = if let Ok(v) = env::var("CLIPPER_GSR_AUDIO") {
        v.split('|')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else if let Ok(path) = crate::config::gsr_audio_file()
        && let Ok(txt) = fs::read_to_string(path)
    {
        let v = parse_audio_list(&txt);
        if v.is_empty() {
            default_audio()
        } else {
            filter_available(v)
        }
    } else {
        default_audio()
    };
    apply_audio_flags(list)
}

/// Nothing configured: desktop and mic as two separate tracks rather than one
/// mixed blob, so the mic toggle has something it can actually remove.
fn default_audio() -> Vec<String> {
    vec!["default_output".to_string(), mic_source()]
}

/// Replay buffer length in seconds.
///
/// Order of precedence: `CLIPPER_GSR_SECONDS`, then the length chosen in the
/// GUI (a state file), then 300. The GUI writes the state file; the env var
/// exists so a shell or a systemd drop-in can still override everything.
pub fn seconds() -> u32 {
    if let Some(v) = env::var("CLIPPER_GSR_SECONDS").ok().and_then(|s| s.parse().ok()) {
        return v;
    }
    crate::state::gsr_seconds().unwrap_or(300)
}

/// Constant bitrate in kbps. Constant (not quality-based) keeps the replay
/// buffer's RAM usage predictable, which is what you want while gaming, and is
/// what gsr's own man page recommends for a replay buffer.
///
/// 40 Mbps at 1440p60 is about double what YouTube aims at when it re-encodes a
/// 1440p60 upload (~20 Mbps), and that headroom is the point: their encoder
/// keeps whatever artefacts the source already has and then spends bits
/// re-encoding them, so a clean source is strictly better input — but bits
/// beyond roughly twice their target are discarded on upload anyway. This used
/// to be 80, which bought nothing measurable. Halving it takes a 3-minute clip
/// from ~1.7 GB to ~0.9 GB, and the buffer's RAM footprint halves with it.
pub fn bitrate_kbps() -> u32 {
    env::var("CLIPPER_GSR_BITRATE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40_000)
}

/// Frame rate. 60 matches the monitors; pinned rather than left to gsr's own
/// default so a future upstream change cannot quietly halve the clips.
pub fn fps() -> u32 {
    env::var("CLIPPER_GSR_FPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60)
}

/// Capture resolution. `0x0` means "the monitor's own resolution", which is
/// what "native" should mean: hard-coding 2560x1440 would silently downscale
/// the day a different monitor becomes primary.
pub const SIZE: &str = "0x0";

/// Video codec. h264 because the clips go into an editor and every editor
/// reads it. The RX 6700 also encodes hevc, which is better per bit, but a few
/// editors choke on hevc inside mp4 and a clip that will not open is worse than
/// a slightly larger one.
pub const CODEC: &str = "h264";

/// "auto" means: ask gsr for its first monitor. Hard-coding a connector name
/// here would break the moment the cable moves to another port.
pub fn monitor() -> Result<String> {
    let m = var("CLIPPER_GSR_MONITOR", "auto");
    if m != "auto" && !m.is_empty() {
        return Ok(m);
    }
    let out = Command::new(BIN)
        .arg("--list-monitors")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .with_context(|| format!("running {BIN} --list-monitors"))?;
    parse_monitors(&String::from_utf8_lossy(&out.stdout))
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("{BIN} reported no monitors"))
}

/// Where replays land: `CLIPPER_GSR_DIR`, then `gsr_dir.txt`, then
/// `~/Videos/clips`.
pub fn output_dir() -> PathBuf {
    if let Ok(d) = env::var("CLIPPER_GSR_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(path) = gsr_dir_file()
        && let Ok(txt) = fs::read_to_string(path)
    {
        let t = txt.trim();
        if !t.is_empty() {
            return PathBuf::from(t);
        }
    }
    dirs::home_dir()
        .map(|h| h.join("Videos/clips"))
        .unwrap_or_else(|| PathBuf::from("/tmp/clips"))
}

/// True when EasyEffects is up and has published its filtered virtual mic.
///
/// The node only exists while EasyEffects is running, so this is a live check
/// rather than a config flag — it also covers the race where GSR is started
/// before EasyEffects has finished logging in.
fn easyeffects_source_present() -> bool {
    Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("easyeffects_source"))
}

/// Which microphone GSR should record.
///
/// EasyEffects does not filter "for the recorder"; it publishes a virtual mic
/// (`easyeffects_source`) and every app has to name it. PipeWire's own default
/// is still the raw hardware mic, which is why GSR recorded unfiltered audio
/// while your apps sounded processed. Falls back to the raw default when
/// EasyEffects is absent, because naming a node that does not exist means
/// recording no mic at all rather than falling back on its own.
pub fn mic_source() -> String {
    if easyeffects_source_present() {
        "easyeffects_source".to_string()
    } else {
        "default_input".to_string()
    }
}

/// Parse `--list-monitors` output, which is one `NAME|WIDTHxHEIGHT` per line.
pub fn parse_monitors(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split('|').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// True if `argv[0]` from a `/proc/<pid>/cmdline` buffer is our recorder.
fn cmdline_is_gsr(raw: &[u8]) -> bool {
    let Some(arg0) = raw.split(|&b| b == 0).next() else {
        return false;
    };
    Path::new(String::from_utf8_lossy(arg0).as_ref())
        .file_name()
        .map(|n| n == BIN)
        .unwrap_or(false)
}

/// Find a running recorder. `/proc/<pid>/comm` is truncated to 15 characters,
/// which would cut this binary's name off, so argv[0] from cmdline is the
/// honest place to look.
pub fn pid() -> Option<u32> {
    let me = std::process::id();
    for entry in fs::read_dir("/proc").ok()? {
        let name = entry.ok()?.file_name();
        let Some(p) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if p == me {
            continue;
        }
        if let Ok(raw) = fs::read(format!("/proc/{p}/cmdline"))
            && cmdline_is_gsr(&raw)
        {
            return Some(p);
        }
    }
    None
}

fn signal(pid: u32, sig: &str) -> Result<()> {
    let status = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()
        .context("running kill(1)")?;
    if !status.success() {
        return Err(anyhow!("kill -{sig} {pid} failed"));
    }
    Ok(())
}

/// Ask the running recorder to write the replay buffer to disk.
pub fn save() -> Result<u32> {
    let p = pid().ok_or_else(|| anyhow!("{BIN} is not running"))?;
    signal(p, "USR1")?;
    Ok(p)
}

/// Stop recording. Graceful, so the current file is finalised properly.
pub fn stop() -> Result<u32> {
    let p = pid().ok_or_else(|| anyhow!("{BIN} is not running"))?;
    signal(p, "INT")?;
    Ok(p)
}

/// Start a replay-buffer recorder in the background. It outlives this process
/// on purpose: one recorder, many triggers (button, CLI, voice later).
/// If one is already running, that pid is returned unchanged.
pub fn start() -> Result<u32> {
    if let Some(p) = pid() {
        return Ok(p);
    }
    let monitor = monitor()?;
    let dir = output_dir();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    let seconds = seconds().to_string();
    let bitrate = bitrate_kbps().to_string();
    let fps = fps().to_string();
    let audio = audio_list();
    let mut cmd = Command::new(BIN);
    cmd.args(["-w", &monitor])
        .args(["-c", "mp4"])
        .args(["-r", &seconds])
        .args(["-s", SIZE])
        .args(["-f", &fps])
        .args(["-k", CODEC])
        .args(["-bm", "cbr"])
        .args(["-q", &bitrate]);
    // One flag per source, not one flag with `|` between them: that is the
    // difference between N separate tracks and one mixed track.
    for source in &audio {
        cmd.args(["-a", source]);
    }
    let child = cmd
        // In replay mode -o is an output *directory*: gsr names each file
        // Replay_<timestamp>.mp4 itself. Passing a file name here makes it
        // create a directory with that name.
        .arg("-o")
        .arg(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // gsr's own error text is the only clue when it refuses to start, and
        // swallowing it cost a whole debugging round. Overwritten each start, so
        // it always holds exactly the last attempt and cannot grow. Failing to
        // open it must never stop recording, so fall back to discarding stderr
        // rather than unwrapping — a panic here would take the listener with it.
        .stderr(
            crate::config::state_dir()
                .ok()
                .and_then(|d| {
                    std::fs::create_dir_all(&d).ok()?;
                    std::fs::File::create(d.join("gsr.log")).ok()
                })
                .map(Stdio::from)
                .unwrap_or_else(Stdio::null),
        )
        .spawn()
        .with_context(|| format!("starting {BIN} on {monitor}"))?;
    Ok(child.id())
}

/// One-line description of the current recorder state, for status output.
pub fn status_line() -> String {
    match pid() {
        Some(p) => format!("gpu-screen-recorder running (pid {p}), {}s buffer", seconds()),
        None => "gpu-screen-recorder not running".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_list_skips_comments_and_blank_lines() {
        let txt = "# my sources\n\neasyeffects_source\n  app:cs2  # my game\n\n#app:skipped\n";
        assert_eq!(parse_audio_list(txt), vec!["easyeffects_source", "app:cs2"]);
    }

    #[test]
    fn empty_audio_list_body_is_not_used_as_a_setting() {
        // A file of only comments must fall through to the default rather than
        // starting gsr with no audio at all.
        assert!(parse_audio_list("# nothing here\n").is_empty());
    }

    #[test]
    fn monitors_parse_to_names() {
        let out = "DP-1|2560x1440\nDP-2|2560x1440\nHDMI-A-1|1920x1080\n";
        assert_eq!(parse_monitors(out), vec!["DP-1", "DP-2", "HDMI-A-1"]);
    }

    #[test]
    fn monitor_parse_tolerates_noise() {
        assert_eq!(parse_monitors("\n\nDP-1|1x1\n"), vec!["DP-1"]);
        assert!(parse_monitors("").is_empty());
    }

    #[test]
    fn cmdline_match_needs_the_exact_binary_name() {
        assert!(cmdline_is_gsr(b"gpu-screen-recorder\0-w\0DP-1\0".as_slice()));
        // A grep, or the gtk front-end, must not be mistaken for the recorder.
        assert!(!cmdline_is_gsr(b"grep\0gpu-screen-recorder\0".as_slice()));
        assert!(!cmdline_is_gsr(b"gpu-screen-recorder-gtk\0".as_slice()));
        assert!(!cmdline_is_gsr(b"".as_slice()));
    }

    #[test]
    fn defaults_are_sane() {
        // Only meaningful because no CLIPPER_GSR_* var is set during tests.
        assert_eq!(bitrate_kbps(), 40_000);
        assert_eq!(fps(), 60);
        assert_eq!(SIZE, "0x0");
        assert_eq!(CODEC, "h264");
        // The mic half depends on what is running, so check the shape only.
        assert_eq!(default_audio()[0], "default_output");
        assert!(matches!(
            mic_source().as_str(),
            "easyeffects_source" | "default_input"
        ));
    }

    #[test]
    fn mic_sources_are_told_apart_from_desktop_sound() {
        assert!(is_mic_source("easyeffects_source"));
        assert!(is_mic_source("default_input"));
        assert!(is_mic_source(
            "alsa_input.pci-0000_09_00.4.analog-stereo",
        ));
        assert!(!is_mic_source("default_output"));
        assert!(!is_mic_source("app:cs2"));
        assert!(!is_mic_source("app:discord"));
        // A monitor of an *output* is still desktop sound, despite the name.
        assert!(!is_mic_source(
            "alsa_output.pci-0000_09_00.4.analog-stereo.monitor"
        ));
    }

    #[test]
    fn audio_flags_drop_only_the_side_they_control() {
        let all: Vec<String> = ["app:cs2", "app:discord", "easyeffects_source", "default_output"]
            .into_iter()
            .map(String::from)
            .collect();

        // Both on is the default and drops nothing.
        assert_eq!(filter_audio(all.clone(), true, true).len(), 4);
        // Mic off still records the game and Discord.
        assert_eq!(
            filter_audio(all.clone(), false, true),
            vec!["app:cs2", "app:discord", "default_output"]
        );
        // Desktop off leaves the mic alone.
        assert_eq!(filter_audio(all.clone(), true, false), vec!["easyeffects_source"]);
        // Both off hands gsr no -a flag at all, which is silent video.
        assert!(filter_audio(all, false, false).is_empty());
    }
}
