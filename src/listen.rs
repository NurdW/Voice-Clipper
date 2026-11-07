// SPDX-License-Identifier: GPL-3.0-or-later
//! The always-on listener: mic -> Vosk keyword spotting -> replay save.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::config::{self, build_grammar, load_phrases};
use crate::mic::{self, Mic};
use crate::state;
use crate::vosk::{Speech, result_text};

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
    eprintln!("clipper: listening. {} phrases armed. Ctrl-C to stop.", phrases.len());
    for p in &phrases {
        eprintln!("  • {p}");
    }

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let target = mic::target_from_env();
    eprintln!(
        "clipper: mic source: {}",
        target.clone().unwrap_or_else(|| "system default input".to_string())
    );
    let mut mic = Mic::start(SAMPLE_RATE as u32, target.as_deref())?;
    let mut last_fire = Instant::now() - COOLDOWN;
    let mut last_mute_check = Instant::now() - Duration::from_secs(1);
    let mut last_buffer_check = Instant::now() - ENSURE_INTERVAL;
    let mut muted_logged = false;

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

        // Re-check the mute flag about once a second. Cheap stat, no IPC needed,
        // and it means `clipctl mute` / the GUI / a hotkey all just work.
        if last_mute_check.elapsed() >= Duration::from_millis(500) {
            last_mute_check = Instant::now();
            match state::is_muted() {
                Ok(true) => {
                    if !muted_logged {
                        eprintln!("clipper: muted — ignoring speech (audio still being read)");
                        muted_logged = true;
                    }
                    speech.reset();
                    continue;
                }
                Ok(false) => muted_logged = false,
                Err(e) => eprintln!("clipper: could not read mute state: {e}"),
            }
        }

        if !speech.accept(&buf) {
            continue;
        }

        let text = result_text(&speech.result());
        if text.is_empty() {
            continue;
        }
        eprintln!("clipper: heard \"{text}\"");

        if last_fire.elapsed() < COOLDOWN {
            continue;
        }
        if let Some(hit) = phrases.iter().find(|p| phrase_in(&text, p)) {
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

/// True when `phrase` appears in `text` as a run of whole words, in order.
///
/// Word-level (not substring) so "clip that" does not fire on "unclip thatx".
///
/// **No wildcards.** Every word of the phrase must be recognised, exactly. A
/// previous version let `[unk]` stand in for the wake word, on the theory that
/// the small model reliably muffles "chat"; in practice that tolerance mostly
/// fired on noise, so it is gone. The trade is deliberate: more missed triggers
/// when you mumble, no clips you did not ask for.
///
/// Note this makes the matcher strict about what Vosk emits, not about how you
/// speak — "chat clip that" said clearly but transcribed as "chat clip that's"
/// will not fire either.
fn phrase_in(text: &str, phrase: &str) -> bool {
    let t: Vec<&str> = text.split_whitespace().collect();
    let p: Vec<&str> = phrase.split_whitespace().collect();
    if p.is_empty() || t.len() < p.len() {
        return false;
    }
    t.windows(p.len())
        .any(|window| window.iter().zip(&p).all(|(tw, pw)| *tw == *pw))
}

#[cfg(test)]
mod tests {
    use super::phrase_in;

    #[test]
    fn matches_whole_word_runs_only() {
        assert!(phrase_in("clip that", "clip that"));
        assert!(phrase_in("alright chat clip that now", "clip that"));
        assert!(phrase_in("hey jarvis clip that", "clip that"));
        assert!(!phrase_in("clip", "clip that"));
        assert!(!phrase_in("that clip", "clip that"));
        assert!(!phrase_in("", "clip that"));
    }

    #[test]
    fn multiword_phrase_needs_all_words_in_order() {
        assert!(phrase_in("hey jarvis clip that", "hey jarvis clip that"));
        assert!(!phrase_in("jarvis hey clip that", "hey jarvis clip that"));
    }

    #[test]
    fn no_wildcard_is_tolerated() {
        // Every word must be heard. These are real journal lines that used to
        // save clips with a word missing.
        assert!(phrase_in("chat clip that", "chat clip that"));
        assert!(phrase_in("hey chat clip that", "chat clip that"));
        assert!(!phrase_in("[unk] clip that", "chat clip that"));
        assert!(!phrase_in("chat clip [unk]", "chat clip that"));
        assert!(!phrase_in("chat did you [unk] that", "chat did you see that"));
        assert!(!phrase_in("take chat take [unk]", "chat take notes"));
        assert!(!phrase_in("chat take [unk]", "chat take notes"));
        assert!(!phrase_in("[unk] [unk] take notes [unk]", "chat take notes"));
    }

    #[test]
    fn a_missing_wake_word_does_not_fire() {
        // No [unk] at all means nothing was heard in the wake-word position, so
        // the bare phrase must not trigger — that is what the wake word is for.
        assert!(!phrase_in("clip that", "chat clip that"));
        assert!(!phrase_in("alright clip that now", "chat clip that"));
        assert!(!phrase_in("take notes", "chat take notes"));
    }

    #[test]
    fn all_unk_is_not_a_match() {
        // Otherwise every burst of background noise would match every phrase.
        assert!(!phrase_in("[unk] [unk] [unk]", "chat clip that"));
        assert!(!phrase_in("[unk] [unk] [unk] [unk]", "chat did you see that"));
    }

    #[test]
    fn single_word_phrases_need_an_exact_word() {
        assert!(phrase_in("reddit", "reddit"));
        assert!(phrase_in("go reddit now", "reddit"));
        assert!(!phrase_in("[unk]", "reddit"));
    }
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
        .args(["-v", "error", "-i", path, "-f", "s16le", "-ac", "1", "-ar", "16000", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not run ffmpeg (install it, or feed a 16 kHz mono WAV): {e}"))?
        ;
    let mut stdout = child.stdout.take().expect("ffmpeg stdout pipe");

    let mut buf = vec![0i16; CHUNK_SAMPLES];
    let mut heard_any = false;
    let mut matched: Vec<String> = Vec::new();

    loop {
        let bytes = {
            use std::io::Read;
            let slice =
                unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr() as *mut u8, buf.len() * 2) };
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
            let hit = phrases.iter().find(|p| phrase_in(&text, p));
            println!("heard: {text}{}", hit.map(|h| format!("   → MATCH \"{h}\"")).unwrap_or_default());
            if let Some(h) = hit {
                matched.push(h.clone());
            }
        }
    }

    // Flush whatever is still in the recognizer at the end of the file.
    let tail = result_text(&speech.final_result());
    if !tail.is_empty() {
        heard_any = true;
        let hit = phrases.iter().find(|p| phrase_in(&tail, p));
        println!("heard: {tail}{}", hit.map(|h| format!("   → MATCH \"{h}\"")).unwrap_or_default());
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
        println!("{} phrase(s) matched — a live clip would have fired.", matched.len());
    }
    Ok(())
}
