// SPDX-License-Identifier: GPL-3.0-or-later
//! clipctl — voice-triggered game clipping for Linux, on GPU Screen Recorder.
//!
//! What it actually does: gpu-screen-recorder keeps a rolling replay buffer in
//! RAM. This tool listens to your microphone entirely offline (Vosk, restricted
//! grammar) and, when you say one of your phrases, signals gsr to write that
//! buffer out as a file. No cloud, no upload, no Windows.
//!
//! Run `clipctl doctor` first if anything seems off; it checks every
//! prerequisite and tells you which one is missing.

use clap::{Parser, Subcommand};

use anyhow::Result;
use std::io::Write;
use clipper::config;
use clipper::{gsr, listen, mic, state};

/// Print that survives a reader hanging up.
///
/// `println!` panics on a write error, so `clipctl status | head` used to end
/// with a broken-pipe backtrace instead of the first few lines you asked for.
macro_rules! say {
    ($($arg:tt)*) => {{ let _ = writeln!(std::io::stdout(), $($arg)*); }};
}

#[derive(Parser)]
#[command(name = "clipctl", version, about = "Voice-triggered game clipping (offline)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Save a clip right now (the last N seconds of gameplay).
    Clip,
    /// Show the recorder state and whether the listener is armed.
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
    /// Control GPU Screen Recorder, the recorder this tool drives.
    Gsr {
        #[command(subcommand)]
        action: GsrAction,
    },
    /// Read or change how far back the replay buffer reaches.
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

/// The buffer length lives in our own state file, and gsr only reads it when it
/// launches, so changing it means restarting the recorder if one is running.
fn set_buffer(seconds: Option<u32>) -> Result<()> {
    let Some(secs) = seconds else {
        let live = gsr::seconds();
        say!("replay buffer is {live} seconds ({})", human_secs(live));
        say!("clips to   : {}", gsr::output_dir().display());
        return Ok(());
    };
    state::set_gsr_seconds(secs)?;
    // Only claim a restart if a different process came back; `start()` happily
    // returns the pid of one that never died.
    let note = match gsr::pid() {
        None => "",
        Some(previous) => match gsr::stop().and_then(|_| gsr::start()) {
            Ok(new_pid) if new_pid != previous => " and restarted",
            _ => " (applies when GSR next starts)",
        },
    };
    say!(
        "replay buffer set to {secs} seconds ({}){note}",
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
    gsr::save()?;
    say!("clip saved to {}", gsr::output_dir().display());
    if let Err(e) = clipper::sound::play() {
        eprintln!("clipctl: could not play the clip sound: {e:#}");
    }
    Ok(())
}

fn status() -> Result<()> {
    say!("recorder   : {}", gsr::status_line());
    say!("buffer     : {} seconds", gsr::seconds());
    say!("clips to   : {}", gsr::output_dir().display());
    let mic_on = state::gsr_mic_enabled()?;
    let desktop_on = state::gsr_desktop_enabled()?;
    say!(
        "audio      : {}",
        match (mic_on, desktop_on) {
            (true, true) => "mic + desktop".to_string(),
            (true, false) => "mic only".to_string(),
            (false, true) => "desktop only".to_string(),
            (false, false) => "none (silent video)".to_string(),
        }
    );
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

    // gpu-screen-recorder is the recorder now, so everything below is about it.
    let gsr_version = std::process::Command::new(gsr::BIN)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    check(
        "gpu-screen-recorder present",
        gsr_version.is_some(),
        "Fedora: sudo dnf copr enable brycensranch/gpu-screen-recorder-git && sudo dnf install gpu-screen-recorder",
    );

    if let Some(v) = gsr_version {
        say!("[ok  ] gpu-screen-recorder {v}");
        match gsr::monitor() {
            Ok(m) => say!("       capture target: {m}"),
            Err(e) => check("a screen gsr can capture", false, &format!("{e:#}")),
        }
        let dir = gsr::output_dir();
        check(
            "clip folder exists",
            dir.is_dir(),
            &format!(
                "mkdir -p {} (or point CLIPPER_GSR_DIR at a folder that exists)",
                dir.display()
            ),
        );
        say!("       clips go to: {}", dir.display());
        if gsr::pid().is_none() {
            say!("[note] gpu-screen-recorder is not running right now.");
            say!("       Start it with the GUI's clip button or `clipctl gsr start`, then tick");
            say!("       'keep GSR buffer on' so the listener brings it back from now on.");
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
