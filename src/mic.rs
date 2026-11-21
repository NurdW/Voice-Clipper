// SPDX-License-Identifier: GPL-3.0-or-later
//! Microphone capture via `pw-record`.
//!
//! Why a subprocess instead of an audio crate: PipeWire already gives us a
//! one-line, dependency-free raw PCM source (`pw-record --rate 16000
//! --channels 1 -`), it follows whatever the desktop default input is, and it
//! shares the mic happily with Discord/Steam. Fewer Rust deps, fewer surprises.

use std::io::Read;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result};

pub struct Mic {
    child: Child,
    stdout: std::process::ChildStdout,
}

/// Optional PipeWire node to listen to (`CLIPPER_MIC=alsa_input.usb-...`).
/// This exists so clipper can follow one specific microphone without changing
/// the system default input, which every other app would inherit.
pub fn target_from_env() -> Option<String> {
    std::env::var("CLIPPER_MIC")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Which node the listener should capture, right now.
pub fn listen_target() -> Option<String> {
    choose_target(target_from_env(), crate::gsr::easyeffects_source_present())
}

/// The rule, split out so it can be tested without a live PipeWire.
///
/// `CLIPPER_MIC` wins outright: it is the escape hatch, and the way to put the
/// raw mic back in one line. Otherwise prefer EasyEffects' filtered mic. Its gate
/// drops everything below about -45 dBFS, and the listener has no energy floor of
/// its own, so speaker bleed quiet enough that whisper.cpp returns `[INAUDIBLE]`
/// was being forced by the restricted grammar into a real-looking phrase.
///
/// When EasyEffects is not there, name `default_input` rather than passing no
/// target at all: pw-record fails outright on a node that does not exist, so the
/// fallback has to be resolved before we start it. EasyEffects is a systemd unit
/// with `Restart=always`, so "not there" is a few seconds at login or after a
/// crash, and the listener re-resolves every 30s and moves back on its own.
fn choose_target(override_: Option<String>, easyeffects_present: bool) -> Option<String> {
    if let Some(v) = override_ {
        return Some(v);
    }
    let name = if easyeffects_present { "easyeffects_source" } else { "default_input" };
    Some(name.to_string())
}

/// Record for `secs` seconds and report a robust loudness figure: the 99.5th
/// percentile of |sample| (0..32767).
///
/// `clipctl doctor` uses this as a hint, not a verdict: a dead input and an
/// empty room give the same flat number, so the caller must not claim the mic
/// is broken just because nobody spoke during the window. What it does catch is
/// a capture path that produces nothing but noise floor while someone is
/// talking — which is what a misaligned byte stream looks like.
///
/// A percentile rather than the absolute peak because opening an analog stream
/// can produce one full-scale pop, which would otherwise read as "mic is fine".
pub fn peak_amplitude(rate: u32, secs: u32) -> Result<i64> {
    let mut mic = Mic::start(rate, target_from_env().as_deref())?;
    let mut buf = vec![0i16; rate as usize];
    let mut levels: Vec<i64> = Vec::with_capacity((rate * secs.max(1)) as usize);
    for _ in 0..secs.max(1) {
        mic.read(&mut buf)?;
        levels.extend(buf.iter().map(|&s| (s as i64).abs()));
    }
    // Drop the first 200ms: stream-start transients live there.
    let skip = (rate as usize / 5).min(levels.len());
    levels.drain(..skip);
    if levels.is_empty() {
        return Ok(0);
    }
    levels.sort_unstable();
    let idx = ((levels.len() as f64) * 0.995) as usize;
    Ok(levels[idx.min(levels.len() - 1)])
}

impl Mic {
    /// Start capture. `target` is an optional PipeWire node name; None = the
    /// system default input, which is what you want after changing mics in KDE.
    pub fn start(rate: u32, target: Option<&str>) -> Result<Self> {
        let mut cmd = Command::new("pw-record");
        cmd.arg("--rate")
            .arg(rate.to_string())
            .arg("--channels")
            .arg("1")
            .arg("--format")
            .arg("s16")
            // CRITICAL: without --raw, pw-record writes a 24-byte Sun AU header
            // (".snd") to stdout, which shifts the whole stream so every sample
            // is read misaligned. Bytes still flow, so everything looks healthy
            // while Vosk is fed noise and never recognises a word. Passing a
            // file argument hides this, because a .raw extension implies raw
            // mode; stdout does not.
            .arg("--raw")
            .arg("--latency")
            .arg("50ms");
        if let Some(t) = target {
            cmd.arg("--target").arg(t);
        }
        cmd.arg("-");
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut child = cmd.spawn().context("could not start pw-record (is pipewire running?)")?;
        let stdout = child
            .stdout
            .take()
            .context("pw-record gave us no stdout pipe")?;
        Ok(Self { child, stdout })
    }

    /// Read exactly `buf.len()` bytes of PCM. Errors on EOF, which means
    /// pw-record died and the caller should restart it.
    pub fn read(&mut self, buf: &mut [i16]) -> Result<()> {
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2)
        };
        loop {
            match self.stdout.read_exact(bytes) {
                Ok(()) => return Ok(()),
                // A signal (SIGTERM from systemd, Ctrl-C) interrupts the blocking
                // read. That is not a mic failure — retry and let the caller's
                // shutdown check pick the signal up on the next pass.
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let status = self.child.try_wait().ok().flatten();
                    return match status {
                        Some(s) => Err(e).with_context(|| format!("pw-record exited with {s}")),
                        None => Err(e).context("pw-record stopped producing audio but is still running"),
                    };
                }
            }
        }
    }

    /// True if the capture process is still alive.
    pub fn is_alive(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for Mic {
    fn drop(&mut self) {
        // Never leave an orphaned recorder holding the mic.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::choose_target;

    #[test]
    fn the_filtered_mic_is_preferred_when_it_exists() {
        assert_eq!(choose_target(None, true).as_deref(), Some("easyeffects_source"));
    }

    #[test]
    fn it_falls_back_to_the_named_default_until_easyeffects_returns() {
        // Named, not "no target": pw-record cannot be pointed at a node that is
        // not there, so the fallback has to be decided up front.
        assert_eq!(choose_target(None, false).as_deref(), Some("default_input"));
    }

    #[test]
    fn an_explicit_mic_beats_the_filtered_one() {
        // This is how you put the raw mic back without touching the code.
        let raw = Some("alsa_input.pci-0000_09_00.4.analog-stereo".to_string());
        assert_eq!(choose_target(raw.clone(), true), raw);
        assert_eq!(choose_target(raw.clone(), false), raw);
    }
}
