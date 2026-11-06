// SPDX-License-Identifier: GPL-3.0-or-later
//! clipctl — voice-triggered game clipping for Linux, on top of OBS.
//!
//! What it actually does: OBS keeps a rolling replay buffer in RAM. This tool
//! listens to your microphone entirely offline (Vosk, restricted grammar) and,
//! when you say one of your phrases, asks OBS over its local websocket to
//! write that buffer out as a file. No cloud, no upload, no Windows.
//!
//! Run `clipctl doctor` first if anything seems off; it checks every
//! prerequisite and tells you which one is missing.

use clap::{Parser, Subcommand};

use anyhow::Result;
use std::io::Write;
use clipper::config::{self, ObsWsSettings};
use clipper::{gsr, listen, mic, obs, state};

/// Print that survives a reader hanging up.
///
/// `println!` panics on a write error, so `clipctl status | head` used to end
/// with a broken-pipe backtrace instead of the first few lines you asked for.
macro_rules! say {
    ($($arg:tt)*) => {{ let _ = writeln!(std::io::stdout(), $($arg)*); }};
}

#[derive(Parser)]
#[command(name = "clipctl", version, about = "Voice-triggered OBS clipping (offline)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Save a clip right now (the last N seconds of gameplay).
    Clip,
    /// Show what OBS reports and whether the listener is armed.
    Status,
    /// Stop reacting to speech (audio keeps flowing; nothing is recorded).
    Mute,
    /// Start reacting to speech again.
    Unmute,
    /// Flip listening on/off — bind this to a keyboard shortcut or the GUI.
    Toggle,
    /// Print whether listening is currently on or muted.
    Muted,
    /// Run the voice listener in the foreground (the systemd service runs this).
    Listen,
    /// Test your phrases against an audio file (same recognizer as the listener).
    Hear {
        /// Path to any audio/video file ffmpeg can read
        file: String,
    },
    /// Create ~/.config/clipper/phrases.txt if it does not exist yet.
    Init,
    /// Check every prerequisite and say which one is missing.
    Doctor,
    /// Control GPU Screen Recorder, the low-overhead alternative to OBS.
    Gsr {
        #[command(subcommand)]
        action: GsrAction,
    },
    /// Read or change how far back the OBS replay buffer reaches.
    Buffer {
        /// Seconds, e.g. 180 for three minutes. Omit to just report it.
        seconds: Option<u32>,
    },
    /// Play the clip sound once, so you can check it is audible.
    SoundTest,
}

#[derive(Subcommand)]
enum GsrAction {
    /// Start a replay buffer in the background (does nothing if one is running).
    Start,
    /// Write the last few seconds to disk.
    Save,
    /// Stop recording.
    Stop,
    /// Report whether it is running and where its clips go.
    Status,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Clip => clip(),
        Command::Status => status(),
        Command::Mute => set_muted(true),
        Command::Unmute => set_muted(false),
        Command::Toggle => toggle_and_print(),
        Command::Muted => print_muted(),
        Command::Listen => {
            listen::install_signal_handlers();
            listen::run()
        }
        Command::Hear { file } => listen::hear(&file),
        Command::Init => init(),
        Command::Doctor => doctor(),
        Command::Gsr { action } => gsr_cmd(action),
        Command::Buffer { seconds } => set_buffer(seconds),
        Command::SoundTest => match clipper::sound::sound_file() {
            Ok(f) => {
                say!("playing {}", f.display());
                clipper::sound::play()
            }
            Err(e) => Err(e),
        },
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Buffer length lives in the OBS profile, so this goes through OBS and then
/// restarts the buffer, because OBS only reads RecRBTime when the buffer starts.
fn set_buffer(seconds: Option<u32>) -> Result<()> {
    let current = obs::Obs::current_buffer_seconds()?;
    let Some(secs) = seconds else {
        match current {
            Some(s) => say!("replay buffer is {s} seconds ({})", human_secs(s as u32)),
            None => say!("OBS did not report a buffer length"),
        }
        return Ok(());
    };
    let profile = obs::Obs::apply_buffer_seconds(secs)?;
    say!(
        "replay buffer set to {secs} seconds ({}) in profile '{profile}' and restarted",
        human_secs(secs)
    );
    Ok(())
}

fn human_secs(secs: u32) -> String {
    if secs >= 60 && secs.is_multiple_of(60) {
        format!("{} minute{}", secs / 60, if secs == 60 { "" } else { "s" })
    } else {
        format!("{secs} seconds")
    }
}

/// GPU Screen Recorder is controlled by signals, not a socket, so these are
/// thin wrappers around start/kill.
fn gsr_cmd(action: GsrAction) -> Result<()> {
    match action {
        GsrAction::Start => {
            let pid = gsr::start()?;
            say!(
                "gpu-screen-recorder running (pid {pid}) — {}s buffer, clips in {}",
                gsr::seconds(),
                gsr::output_dir().display()
            );
        }
        GsrAction::Save => {
            let pid = gsr::save()?;
            say!(
                "replay saved to {} (by pid {pid})",
                gsr::output_dir().display()
            );
        }
        GsrAction::Stop => {
            let pid = gsr::stop()?;
            say!("gpu-screen-recorder stopping (pid {pid})");
        }
        GsrAction::Status => {
            say!("{}", gsr::status_line());
            say!("clips : {}", gsr::output_dir().display());
        }
    }
    Ok(())
}

fn clip() -> Result<()> {
    let settings = ObsWsSettings::load()?;
    let mut obs = obs::Obs::connect(&settings)?;
    obs.save_replay()?;
    say!("clip saved");
    if let Err(e) = clipper::sound::play() {
        eprintln!("clipctl: could not play the clip sound: {e:#}");
    }
    Ok(())
}

fn status() -> Result<()> {
    let settings = ObsWsSettings::load()?;
    let mut obs = obs::Obs::connect(&settings)?;
    say!("connection : {}", obs.versions()?);
    say!(
        "replay buf : {}",
        if obs.replay_active()? {
            "running"
        } else {
            "NOT running (start it in OBS, or launch OBS with --startreplaybuffer)"
        }
    );
    say!("clips to   : {}", obs.record_dir()?);
    say!("listening  : {}", if state::is_muted()? { "muted" } else { "on" });
    match config::load_phrases() {
        Ok(p) => say!("phrases    : {}", p.len()),
        Err(e) => say!("phrases    : not set up ({e})"),
    }
    match config::find_model() {
        Ok(m) => say!("model      : {}", m.display()),
        Err(_) => say!("model      : missing (see clipctl doctor)"),
    }
    Ok(())
}

fn set_muted(muted: bool) -> Result<()> {
    state::set_muted(muted)?;
    say!("{}", if muted { "muted" } else { "listening" });
    Ok(())
}

fn toggle_and_print() -> Result<()> {
    say!("{}", if state::toggle()? { "muted" } else { "listening" });
    Ok(())
}

fn print_muted() -> Result<()> {
    say!("{}", if state::is_muted()? { "muted" } else { "listening" });
    Ok(())
}

fn init() -> Result<()> {
    config::ensure_dirs()?;
    let path = config::phrases_path()?;
    if path.exists() {
        say!("already exists: {}", path.display());
        return Ok(());
    }
    std::fs::write(&path, DEFAULT_PHRASES)
        .map_err(|e| anyhow::anyhow!("could not write {}: {e}", path.display()))?;
    say!("wrote {}", path.display());
    say!("edit that file to change what you say; then restart the listener.");
    Ok(())
}

const DEFAULT_PHRASES: &str = r#"# One trigger phrase per line. Lowercase, plain words.
# Say the phrase out loud; the last N seconds of gameplay get saved.
# Restart the listener after editing:  systemctl --user restart clipper
clip that
chat clip that
shit clip that
reddit
hey jarvis clip that
"#;

fn doctor() -> Result<()> {
    let mut problems = 0usize;
    let mut check = |label: &str, ok: bool, hint: &str| {
        say!("[{}] {}", if ok { "ok  " } else { "MISS" }, label);
        if !ok {
            problems += 1;
            say!("       → {hint}");
        }
    };

    check(
        "pw-record present (PipeWire audio capture)",
        std::process::Command::new("pw-record")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok(),
        "sudo dnf install pipewire-utils",
    );
    check(
        "libvosk.so present",
        std::path::Path::new("/usr/lib64/libvosk.so").exists() || std::path::Path::new("/usr/lib/libvosk.so").exists(),
        "sudo dnf install vosk-api-devel",
    );
    check(
        "Vosk speech model downloaded",
        config::find_model().is_ok(),
        &format!(
            "mkdir -p {d} && cd {d} && curl -LO https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip && unzip vosk-model-small-en-us-0.15.zip",
            d = config::model_dir()?.display()
        ),
    );
    check(
        "phrases.txt configured",
        config::load_phrases().is_ok(),
        "run: clipctl init",
    );

    let ws = ObsWsSettings::load();
    check(
        "obs-websocket enabled in OBS",
        ws.is_ok(),
        "OBS -> Settings -> WebSocket Server -> enable (and 'Automatically connect')",
    );

    if let Ok(settings) = ws {
        match obs::Obs::connect(&settings) {
            Ok(mut o) => {
                say!("[ok  ] OBS reachable — {}", o.versions()?);
                let active = o.replay_active()?;
                check(
                    "replay buffer running",
                    active,
                    "in OBS click 'Start Replay Buffer', or start OBS with --startreplaybuffer",
                );
                say!("       clips go to: {}", o.record_dir()?);
            }
            Err(e) => check("OBS reachable on localhost", false, &format!("{e:#}")),
        }
    }

    // Probe the actual signal, not just whether pw-record is alive: a muted
    // input still produces a steady stream of bytes, which is why a dead mic
    // can look perfectly healthy for hours.
    print!("[    ] microphone signal (listening 3s — say something now)… ");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    match mic::peak_amplitude(16_000, 3) {
        Ok(peak) => {
            // Deliberately NOT a failure. An empty room and a dead input produce
            // the same flat reading if nobody talks during the window, so the
            // honest answer is "I could not tell" rather than "your mic is muted".
            if peak >= 2_000 {
                say!("ok");
                say!("       typical peak {peak} of 32767 — speech detected");
            } else {
                say!("quiet");
                say!("       typical peak {peak} of 32767.");
                say!("       Nobody spoke during those 3 seconds. That looks identical to a dead");
                say!("       input, so it proves nothing either way — run it again and talk. If it");
                say!("       stays flat while you are speaking, then suspect the mic or its jack.");
            }
        }
        Err(e) => {
            say!("MISS");
            problems += 1;
            say!("       → could not capture from the microphone: {e:#}");
        }
    }

    say!();
    if problems == 0 {
        say!("everything looks good.");
    } else {
        say!("{problems} thing(s) to fix above.");
        std::process::exit(1);
    }
    Ok(())
}
