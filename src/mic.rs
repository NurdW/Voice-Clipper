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
