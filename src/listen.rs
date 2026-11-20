// SPDX-License-Identifier: GPL-3.0-or-later
//! The always-on listener: mic -> Vosk keyword spotting -> replay save.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, build_grammar, load_phrases};
use crate::mic::{self, Mic};
use crate::state;
use crate::vosk::{Speech, result_conf, result_text};

const SAMPLE_RATE: f32 = 16000.0;
/// 100 ms of mono 16-bit audio. Small enough that a clip trigger lands within
/// a tenth of a second of you finishing the phrase.
const CHUNK_SAMPLES: usize = 1600;
/// Don't fire twice in quick succession when Vosk emits the same phrase twice.
const COOLDOWN: Duration = Duration::from_secs(5);
/// How often to double-check that the recorder is actually running.
const ENSURE_INTERVAL: Duration = Duration::from_secs(30);

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn install_signal_handlers() {
    // Safe enough: the handler only stores a bool.
    unsafe {
        libc::signal(libc::SIGINT, on_term as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_term as *const () as libc::sighandler_t);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum ChunkAction {
    Recognize,
    Discard,
    ReopenCapture,
}

struct MuteGate {
    flag: PathBuf,
    muted: bool,
}

impl MuteGate {
    fn new(flag: PathBuf) -> Self {
        let muted = flag.try_exists().unwrap_or(true);
        if muted {
            eprintln!("clipper: starting muted — ignoring speech");
        }
        Self { flag, muted }
    }

    fn chunk_action(&mut self, reset: impl FnOnce()) -> ChunkAction {
        let state = self.flag.try_exists();
        if matches!(state, Ok(false)) {
            // The old pipe can hold multiple chunks captured while muted.
            return if std::mem::take(&mut self.muted) {
                ChunkAction::ReopenCapture
            } else {
                ChunkAction::Recognize
            };
        }
        if !self.muted {
            match state {
                Err(e) => eprintln!("clipper: cannot read mute state: {e}; ignoring speech"),
                _ => eprintln!("clipper: muted — ignoring speech (audio still being read)"),
            }
            reset();
            self.muted = true;
        }
        ChunkAction::Discard
    }
}

pub fn run() -> Result<()> {
    config::ensure_dirs()?;
    let phrases = load_phrases()?;
    let model = config::find_model()?;
    let grammar = build_grammar(&phrases);

    eprintln!(
        "clipper: loading Vosk model {} (this takes a few seconds)…",
        model.display()
    );
    let mut speech = Speech::new(&model, SAMPLE_RATE, &grammar)?;
    eprintln!(
        "clipper: listening. {} phrases armed. Ctrl-C to stop.",
        phrases.len()
    );
    for p in &phrases {
        eprintln!("  • {p}");
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let target = mic::target_from_env();
    eprintln!(
        "clipper: mic source: {}",
        target
            .clone()
            .unwrap_or_else(|| "system default input".to_string())
    );
    let mut mute = MuteGate::new(config::muted_flag()?);
    let mut mic = Mic::start(SAMPLE_RATE as u32, target.as_deref())?;
    let mut last_fire = Instant::now() - COOLDOWN;
    let mut last_buffer_check = Instant::now() - ENSURE_INTERVAL;

    while !STOP.load(Ordering::SeqCst) {
        // gsr only records when something started it, and it can be killed by
        // anything from a session restart to an OOM. Every 30s we make sure the
        // recorder you asked to keep alive actually is.
        if last_buffer_check.elapsed() >= ENSURE_INTERVAL {
            last_buffer_check = Instant::now();
            ensure_gsr_buffer();
        }

        if let Err(e) = mic.read(&mut buf) {
            if STOP.load(Ordering::SeqCst) {
                break; // we were asked to quit; don't restart the mic on the way out
            }
            eprintln!("clipper: mic dropped ({e}); restarting capture");
            sleep(Duration::from_millis(500));
            mic = Mic::start(SAMPLE_RATE as u32, target.as_deref())?;
            continue;
        }

        match mute.chunk_action(|| speech.reset()) {
            ChunkAction::Recognize => {}
            ChunkAction::Discard => continue,
            ChunkAction::ReopenCapture => {
                drop(mic);
                mic = Mic::start(SAMPLE_RATE as u32, target.as_deref())?;
                continue;
            }
        }
        if !speech.accept(&buf) {
            continue;
        }

        let res = speech.result();
        // The raw JSON is the only way to find out what Vosk actually reports:
        // a grammar-restricted recognizer may omit `conf` entirely, and no
        // amount of reading the docs settles it for this build.
        if std::env::var_os("CLIPPER_DEBUG_JSON").is_some() {
            eprintln!("clipper: raw {res}");
        }
        let text = result_text(&res);
        if text.is_empty() {
            continue;
        }
        // Confidence is logged even though nothing acts on it yet: a restricted
        // grammar promotes room noise into real-looking phrases, and the only
        // way to pick a sane floor is to see what the model actually reports.
        match result_conf(&res) {
            Some(c) => eprintln!("clipper: heard \"{text}\" (conf {c:.2})"),
            None => eprintln!("clipper: heard \"{text}\""),
        }

        if last_fire.elapsed() < COOLDOWN {
            continue;
        }
        if let Some(hit) = phrases.iter().find(|p| phrase_is_whole_utterance(&text, p)) {
            last_fire = Instant::now();
            eprintln!("clipper: phrase \"{hit}\" matched → saving clip");
            if let Err(e) = save_clip() {
                eprintln!("clipper: could not save clip: {e:#}");
            }
        }
    }

    eprintln!("clipper: stopping");
    Ok(())
}

/// True when `text` is the phrase and nothing else: same words, same order, no
/// extra words on either side.
///
/// **No wildcards, no padding.** Every word must be recognised exactly, *and*
/// Vosk must have heard nothing else in that utterance. Two earlier versions
/// were looser: one let `[unk]` stand in for the wake word, the next allowed the
/// phrase to sit anywhere inside a longer transcript. Both tolerated too much.
/// A restricted grammar actively promotes room noise into real-looking phrases,
/// and the journal line `[unk] [unk] chat clip that [unk]` saved a clip that
/// nobody asked for. Requiring the whole utterance is the difference between a
/// command and a coincidence.
///
/// Word-level (not substring) so "clip that" does not fire on "unclip thatx".
///
/// The trade is deliberate and it does cost misses: a clean "chat clip that"
/// wrapped in a cough (`[unk] chat clip that`) will not fire. Say the phrase on
/// its own, as a separate breath.
fn phrase_is_whole_utterance(text: &str, phrase: &str) -> bool {
    let t: Vec<&str> = text.split_whitespace().collect();
    let p: Vec<&str> = phrase.split_whitespace().collect();
    !p.is_empty() && t.len() == p.len() && t.iter().zip(&p).all(|(tw, pw)| *tw == *pw)
}

/// Save a clip from GPU Screen Recorder.
fn save_clip() -> Result<()> {
    crate::gsr::save()?;
    eprintln!("clipper: gsr clip saved");
    // The clip is already saved; a silent machine is a nuisance, not a failure.
    if let Err(e) = crate::sound::play() {
        eprintln!("clipper: could not play the clip sound: {e:#}");
    }
    Ok(())
}

/// Start GPU Screen Recorder if you asked to keep it alive and it is not
/// running. There is no "up but idle" state to notice: gsr either records or it
/// does not, so this is a plain start-if-absent.
fn ensure_gsr_buffer() {
    if !state::gsr_auto_enabled().unwrap_or(false) {
        return;
    }
    if crate::gsr::pid().is_some() {
        return;
    }
    match crate::gsr::start() {
        Ok(pid) => eprintln!("clipper: started gpu-screen-recorder (pid {pid})"),
        Err(e) => eprintln!("clipper: could not start gpu-screen-recorder: {e:#}"),
    }
}

/// Run the SAME recognizer the listener uses over an audio file and print what
/// it hears plus whether any of your phrases matched.
///
/// This exists because "does it understand my voice?" is otherwise impossible
/// to test without standing at your mic saying the same sentence over and over.
/// Decode is delegated to ffmpeg so any format you throw at it works.
pub fn hear(path: &str) -> Result<()> {
    use std::process::{Command as StdCommand, Stdio};

    config::ensure_dirs()?;
    let phrases = load_phrases()?;
    let model = config::find_model()?;
    let mut speech = Speech::new(&model, SAMPLE_RATE, &build_grammar(&phrases))?;

    let mut child = StdCommand::new("ffmpeg")
        .args([
            "-v", "error", "-i", path, "-f", "s16le", "-ac", "1", "-ar", "16000", "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!("could not run ffmpeg (install it, or feed a 16 kHz mono WAV): {e}")
        })?;
    let mut stdout = child.stdout.take().expect("ffmpeg stdout pipe");

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let mut heard_any = false;
    let mut matched: Vec<String> = Vec::new();

    loop {
        let bytes = {
            use std::io::Read;
            let slice = unsafe {
                std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2)
            };
            match stdout.read(slice) {
                Ok(n) if n > 0 => n / 2,
                _ => break,
            }
        };
        if speech.accept(&buf[..bytes]) {
            let text = result_text(&speech.result());
            if text.is_empty() {
                continue;
            }
            heard_any = true;
            let hit = phrases.iter().find(|p| phrase_is_whole_utterance(&text, p));
            println!(
                "heard: {text}{}",
                hit.map(|h| format!("   → MATCH \"{h}\""))
                    .unwrap_or_default()
            );
            if let Some(h) = hit {
                matched.push(h.clone());
            }
        }
    }

    // Flush whatever is still in the recognizer at the end of the file.
    let tail = result_text(&speech.final_result());
    if !tail.is_empty() {
        heard_any = true;
        let hit = phrases.iter().find(|p| phrase_is_whole_utterance(&tail, p));
        println!(
            "heard: {tail}{}",
            hit.map(|h| format!("   → MATCH \"{h}\""))
                .unwrap_or_default()
        );
        if let Some(h) = hit {
            matched.push(h.clone());
        }
    }

    let _ = child.wait();
    if !heard_any {
        println!("nothing recognised — is there speech in {path}?");
    } else if matched.is_empty() {
        println!("no phrase matched. Your phrases: {}", phrases.join(" | "));
    } else {
        println!(
            "{} phrase(s) matched — a live clip would have fired.",
            matched.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ChunkAction, MuteGate, phrase_is_whole_utterance as whole};

    #[test]
    fn muted_chunks_never_reach_recognition_and_unmute_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("muted");
        let mut gate = MuteGate::new(flag.clone());
        let mut resets = 0;
        let mut accepted = Vec::new();
        // Read 5 can straddle unmute: it started when the flag was still set.
        for (sample, muted) in [
            (1, false),
            (2, true),
            (3, true),
            (4, true),
            (5, false),
            (6, false),
        ] {
            if muted {
                std::fs::write(&flag, b"").unwrap();
            } else if flag.exists() {
                std::fs::remove_file(&flag).unwrap();
            }
            if gate.chunk_action(|| resets += 1) == ChunkAction::Recognize {
                accepted.push(sample);
            }
        }
        assert_eq!(accepted, [1, 6]);
        assert_eq!(resets, 1);
    }

    #[test]
    fn unmute_reopens_capture_instead_of_processing_the_queued_backlog() {
        use std::collections::VecDeque;

        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("muted");
        std::fs::write(&flag, b"").unwrap();
        let mut gate = MuteGate::new(flag.clone());
        let mut pipe = VecDeque::from([10, 20, 30]);
        std::fs::remove_file(flag).unwrap();

        let mut accepted = Vec::new();
        while let Some(chunk) = pipe.pop_front() {
            match gate.chunk_action(|| {}) {
                ChunkAction::Recognize => accepted.push(chunk),
                ChunkAction::Discard => {}
                ChunkAction::ReopenCapture => pipe = VecDeque::from([40]),
            }
        }
        assert_eq!(accepted, [40]);
    }

    #[test]
    fn unreadable_mute_state_does_not_enable_recognition() {
        let dir = tempfile::tempdir().unwrap();
        let flag = dir.path().join("muted");
        std::os::unix::fs::symlink(&flag, &flag).unwrap();
        assert!(flag.try_exists().is_err());
        let mut gate = MuteGate::new(flag);
        assert_eq!(gate.chunk_action(|| {}), ChunkAction::Discard);
    }

    #[test]
    fn the_phrase_must_be_the_whole_utterance() {
        assert!(whole("chat clip that", "chat clip that"));
        // A real journal line that saved a clip nobody asked for: the words were
        // all there, with noise on both sides of them.
        assert!(!whole("[unk] [unk] chat clip that [unk]", "chat clip that"));
        assert!(!whole("alright chat clip that", "chat clip that"));
        assert!(!whole("chat clip that now", "chat clip that"));
        assert!(!whole("clip", "chat clip that"));
        assert!(!whole("", "chat clip that"));
    }

    #[test]
    fn matches_whole_word_runs_only() {
        assert!(whole("clip that", "clip that"));
        assert!(!whole("clip", "clip that"));
        assert!(!whole("that clip", "clip that"));
        assert!(!whole("", "clip that"));
    }

    #[test]
    fn multiword_phrase_needs_all_words_in_order() {
        assert!(whole("hey jarvis clip that", "hey jarvis clip that"));
        assert!(!whole("jarvis hey clip that", "hey jarvis clip that"));
    }

    #[test]
    fn no_wildcard_is_tolerated() {
        // Every word must be heard. These are real journal lines that used to
        // save clips with a word missing.
        assert!(whole("chat clip that", "chat clip that"));
        assert!(!whole("[unk] clip that", "chat clip that"));
        assert!(!whole("chat clip [unk]", "chat clip that"));
        assert!(!whole("chat did you [unk] that", "chat did you see that"));
        assert!(!whole("chat take [unk]", "chat take notes"));
        assert!(!whole("[unk] [unk] take notes [unk]", "chat take notes"));
    }

    #[test]
    fn a_missing_wake_word_does_not_fire() {
        // No [unk] at all means nothing was heard in the wake-word position, so
        // the bare phrase must not trigger — that is what the wake word is for.
        assert!(!whole("clip that", "chat clip that"));
        assert!(!whole("take notes", "chat take notes"));
    }

    #[test]
    fn all_unk_is_not_a_match() {
        // Otherwise every burst of background noise would match every phrase.
        assert!(!whole("[unk] [unk] [unk]", "chat clip that"));
        assert!(!whole("[unk] [unk] [unk] [unk]", "chat did you see that"));
    }

    #[test]
    fn single_word_phrases_need_an_exact_word() {
        assert!(whole("reddit", "reddit"));
        assert!(!whole("go reddit now", "reddit"));
        assert!(!whole("[unk]", "reddit"));
    }
}
