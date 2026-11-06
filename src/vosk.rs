//! Hand-written FFI bindings to libvosk (Fedora: `vosk-api-devel`).
//!
//! Signatures copied from upstream src/vosk_api.h — the C API is tiny, so a
//! hand-written binding is shorter (and easier to audit) than running bindgen.
//! Result strings are owned by the recognizer, so we must NOT free them.

use std::ffi::{CStr, CString, c_char, c_int, c_short};
use std::path::Path;

use anyhow::{Context, Result, bail};

#[repr(C)]
pub struct VoskModel {
    _priv: [u8; 0],
}

#[repr(C)]
pub struct VoskRecognizer {
    _priv: [u8; 0],
}

unsafe extern "C" {
    fn vosk_model_new(model_path: *const c_char) -> *mut VoskModel;
    fn vosk_model_free(model: *mut VoskModel);
    fn vosk_recognizer_new_grm(model: *mut VoskModel, sample_rate: f32, grammar: *const c_char) -> *mut VoskRecognizer;
    fn vosk_recognizer_set_grm(recognizer: *mut VoskRecognizer, grammar: *const c_char);
    fn vosk_recognizer_accept_waveform_s(recognizer: *mut VoskRecognizer, data: *const c_short, length: c_int) -> c_int;
    fn vosk_recognizer_result(recognizer: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_partial_result(recognizer: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_final_result(recognizer: *mut VoskRecognizer) -> *const c_char;
    fn vosk_recognizer_reset(recognizer: *mut VoskRecognizer);
    fn vosk_recognizer_set_endpointer_delays(
        recognizer: *mut VoskRecognizer,
        t_start_max: f32,
        t_end: f32,
        t_max: f32,
    );
    fn vosk_recognizer_free(recognizer: *mut VoskRecognizer);
    fn vosk_set_log_level(log_level: c_int);
}

/// Owns a Vosk model + one recognizer restricted to a keyword grammar.
///
/// The restricted grammar is the whole trick: Vosk only tries to match the
/// phrases you listed and folds everything else into [unk], which is what keeps
/// CPU usage low enough to leave running during a game.
pub struct Speech {
    model: *mut VoskModel,
    rec: *mut VoskRecognizer,
}

impl Speech {
    pub fn new(model_dir: &Path, sample_rate: f32, grammar: &str) -> Result<Self> {
        // -1 = silent. Level 0 still lets kaldi's LOG lines through, which
        // would flood the systemd journal on every model load.
        unsafe { vosk_set_log_level(-1) };

        let c_path = CString::new(
            model_dir
                .to_str()
                .context("model path contains invalid UTF-8")?,
        )
        .context("model path contains a NUL byte")?;

        // Loading the model takes a couple of seconds and prints nothing at log level 0.
        let model = unsafe { vosk_model_new(c_path.as_ptr()) };
        if model.is_null() {
            bail!("could not load Vosk model from {}", model_dir.display());
        }

        let c_grammar = CString::new(grammar).context("grammar contains a NUL byte")?;
        let rec = unsafe { vosk_recognizer_new_grm(model, sample_rate, c_grammar.as_ptr()) };
        if rec.is_null() {
            unsafe { vosk_model_free(model) };
            bail!("could not create Vosk recognizer (bad grammar?)");
        }

        // Endpointer = the "they've stopped talking" detector. The defaults cut
        // an utterance off a bit eagerly, which is how "clip that" arrives as
        // just "clip". 1.0 s of trailing silence is still instant-feeling but
        // catches a phrase said quickly or with a tiny pause in the middle.
        unsafe { vosk_recognizer_set_endpointer_delays(rec, 5.0, 1.0, 20.0) };

        Ok(Self { model, rec })
    }

    /// Replace the grammar at runtime (used if you edit phrases.txt and send SIGHUP).
    pub fn set_grammar(&mut self, grammar: &str) -> Result<()> {
        let c = CString::new(grammar).context("grammar contains a NUL byte")?;
        unsafe { vosk_recognizer_set_grm(self.rec, c.as_ptr()) };
        Ok(())
    }

    /// Feed 16-bit mono samples. Returns true when Vosk thinks an utterance
    /// ended, in which case `result()` holds the final text for that utterance.
    pub fn accept(&mut self, samples: &[i16]) -> bool {
        if samples.is_empty() {
            return false;
        }
        let ended = unsafe {
            vosk_recognizer_accept_waveform_s(
                self.rec,
                samples.as_ptr() as *const c_short,
                samples.len() as c_int,
            )
        };
        ended == 1
    }

    pub fn result(&self) -> String {
        unsafe { c_to_string(vosk_recognizer_result(self.rec)) }
    }

    pub fn partial(&self) -> String {
        unsafe { c_to_string(vosk_recognizer_partial_result(self.rec)) }
    }

    pub fn final_result(&self) -> String {
        unsafe { c_to_string(vosk_recognizer_final_result(self.rec)) }
    }

    pub fn reset(&mut self) {
        unsafe { vosk_recognizer_reset(self.rec) };
    }
}

impl Drop for Speech {
    fn drop(&mut self) {
        unsafe {
            vosk_recognizer_free(self.rec);
            vosk_model_free(self.model);
        }
    }
}

/// The recognizer owns these buffers; copy into a String, never free.
unsafe fn c_to_string(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

/// Pull the `text` field out of a Vosk JSON result line.
/// Result looks like: {"text": "clip that"} or {"text": "", "emitted": "partial"}
pub fn result_text(json_line: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json_line) else {
        return String::new();
    };
    v["text"].as_str().unwrap_or("").to_string()
}

#[cfg(test)]
mod tests {
    use super::result_text;

    #[test]
    fn extracts_text_field() {
        assert_eq!(result_text(r#"{"text": "clip that"}"#), "clip that");
        assert_eq!(result_text(r#"{"text": "", "emitted": "partial"}"#), "");
    }

    #[test]
    fn bad_json_is_empty_not_a_panic() {
        assert_eq!(result_text("not json at all"), "");
    }
}
