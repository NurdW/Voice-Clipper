# Voice Clipper

Voice-triggered game clipping on Linux, built on
[GPU Screen Recorder](https://git.dec05eba.com/gpu-screen-recorder).
Say `chat clip that` and the last few minutes of gameplay get written to disk.
Fully offline: no cloud, no account, no Windows.

**If you use this rather than develop it, read
[`docs/how-to-short-clip.md`](docs/how-to-short-clip.md)** — that is the
how-to: phrases, clip length, muting, troubleshooting, uninstall.

Medal.tv has no Linux build (their `/download/medal-linux` page serves a
Windows `.exe`), so this reproduces the part that matters — the always-running
replay buffer plus a voice trigger — with software that actually runs on
Wayland.

```
 mic ──pw-record──▶ Vosk (restricted grammar) ──phrase matched──▶ clipctl
                                                                    │
                                                    SIGUSR1 to gpu-screen-recorder
                                                                    │
                                                        gpu-screen-recorder ──▶ clips
```

GPU Screen Recorder records each audio source on its own track, so your mic can
be muted or levelled independently of game sound in any editor — the same
multitrack result OBS is used for, at a fraction of the capture overhead.

## Layout

| Path | What |
|---|---|
| `src/main.rs` | `clipctl` CLI: clip / status / mute / unmute / toggle / listen / hear / init / doctor / gsr / buffer |
| `src/lib.rs` | library target so the GUI and CLI share one implementation |
| `src/gsr.rs` | GPU Screen Recorder control (start / save / stop by signal, `/proc` discovery, per-source audio) |
| `src/vosk.rs` | hand-written FFI to `libvosk` (Fedora `vosk-api-devel`) |
| `src/mic.rs` | mic capture by piping `pw-record` (must pass `--raw`, see gotchas) |
| `src/sound.rs` | plays a short sound after a clip lands |
| `src/listen.rs` | the always-on loop, plus `hear` (offline phrase testing) |
| `src/config.rs` | paths, phrase file, the clip folder, the toggle flag files |
| `src/state.rs` | the mute flag and the recorder toggles (a file's existence — deliberately dumb) |
| `gui/` | `voice-clipper`: power button, clip button, clip-length / keep-alive / audio menu (eframe/egui) |
| `systemd/` | user service for the always-on listener |
| `desktop/` | autostart + taskbar identity file (`@HOME@` is substituted by install.sh) |
| `assets/` | app icon source (PNG) |
| `gui/assets/icon.rgba` | 64x64 RGBA embedded into the binary for the window icon |

## Files it reads and writes

| Path | What |
|---|---|
| `~/.config/clipper/phrases.txt` | your trigger phrases, one per line |
| `~/.config/clipper/gsr_audio.txt` | which audio sources to record, one per line |
| `~/.config/clipper/gsr_dir.txt` | where clips go |
| `~/.local/state/clipper/muted` | present == not listening |
| `~/.local/state/clipper/gsr_auto_on` | present == keep the recorder running |
| `~/.local/state/clipper/gsr_no_mic` | present == microphone is off the clip |
| `~/.local/state/clipper/gsr_no_desktop` | present == game/desktop audio is off the clip |
| `~/.local/state/clipper/gsr_seconds` | clip length chosen in the GUI |
| `~/.local/state/clipper/gsr.log` | stderr of the last recorder start |
| `~/.local/share/clipper/` | the Vosk model and the clip sound |

Environment overrides, all optional: `CLIPPER_GSR_DIR`, `CLIPPER_GSR_SECONDS`,
`CLIPPER_GSR_AUDIO`, `CLIPPER_GSR_BITRATE`, `CLIPPER_GSR_FPS`,
`CLIPPER_GSR_MONITOR`, `VOICE_CLIPPER_POS`.

## Build and test

```bash
cargo build --release
cargo test --release          # grammar JSON, phrase matching, audio filtering, arc geometry
./install.sh                  # installs to ~/.local/bin, enables the user service
```

Requires: `vosk-api-devel` (libvosk + header), `pipewire-utils` (pw-record),
`ffmpeg` (for `clipctl hear`), and `gpu-screen-recorder`. On Fedora:

```bash
sudo dnf copr enable brycensranch/gpu-screen-recorder-git
sudo dnf install gpu-screen-recorder vosk-api-devel pipewire-utils ffmpeg
```

The Vosk model is downloaded to `~/.local/share/clipper/`, not committed here.

## Things that are not obvious (learned the hard way)

**Vosk grammar must be plain JSON.** `["clip that", "[unk]"]`. Escaping the
quotes makes the FST reader fail with `ERROR: Parse: Unknown starting character`.

**`vosk_set_log_level(0)` is not silent.** Kaldi still prints LOG lines at 0;
use `-1`.

**The endpointer default is too eager.** With defaults, a fast "clip that" came
back as just `clip`. `vosk_recognizer_set_endpointer_delays(rec, 5.0, 1.0, 20.0)`
fixed it.

**A restricted grammar makes Vosk invent phrases.** With only three allowed
phrases, the model forces noise into them: a clip fired at 06:46 on
`heard "[unk] chat clip that"`, and whisper found **no speech at all** in the
178 s of clip audio behind it (mean −43.5 dB). So a `heard` line is not evidence
someone spoke. When a trigger looks wrong, transcribe the clip tail rather than
arguing from the log:

```sh
ffmpeg -nostdin -loglevel error -y -sseof -20 -i Clip.mp4 -vn -ac 1 -ar 16000 /tmp/t.wav
transcribe /tmp/t.wav
```

An energy or confidence floor would stop this; deliberately not implemented —
the wake-word rule shipped instead and the behaviour is being watched.

**Phrase matching is exact, with no wildcards.** Every word has to be heard.
`[unk]` in the recognised text never matches, which is why a partial
`[unk] chat clip that` is rejected. That trades missed triggers for zero
phantom clips, which is the right way round when a false clip costs you 300 MB.

**`pw-record` to stdout needs `--raw`.** Without it pw-record writes a 24-byte
Sun AU header (`.snd`) to stdout, shifting the whole PCM stream so every sample
is read misaligned. Nothing errors: bytes flow, the process stays alive, and the
recognizer is fed noise forever. A file argument hides this because a `.raw`
extension implies raw mode — stdout does not. Diagnosed by reading the same
bytes at offset 0 (garbage) and offset 1 (clean).

**A flat mic reading is not proof of a muted mic.** An empty room and a dead
input look identical in a short capture window, so `clipctl doctor` reports
"quiet" instead of blaming hardware.

**A signal is not a mic failure.** SIGTERM interrupts the blocking pipe read with
`EINTR`; retry on `ErrorKind::Interrupted` or shutdown looks like a crash.

**In gsr replay mode `-o` is a directory, not a file.** Passing `-o dir/gsr.mp4`
makes it *create* a directory called `gsr.mp4` and write `Replay_<ts>.mp4`
inside it. Pass the directory.

**gsr puts each `-a` on its own audio track; sources joined with `|` are mixed
into one.** Separate flags produce streams tagged by origin (`name=Applications:
discord`), which is what makes per-source levels possible in an editor. The man
page does not say this; it was confirmed with ffprobe. An app that is not running
still yields a silent track, so track order stays stable across clips.

**A missing audio *device* stops gsr starting at all; a missing app does not.**
`filter_available()` probes `--list-audio-devices` before launch and drops
sources that are not there, because one absent device otherwise means the
recorder refuses to start and you get no clips and no obvious reason.

**`gpu-screen-recorder`'s stderr has to go somewhere.** It was sent to
`/dev/null` for weeks, which swallowed a "not a valid audio device" message and
cost an hour of debugging. It now goes to `~/.local/state/clipper/gsr.log`,
overwritten each start. If the log cannot be opened the recorder still starts
with stderr discarded — a panic there would take the listener with it.

**`WAYLAND_DISPLAY` is not stable across sessions.** wayland-0, wayland-1 and
wayland-2 have all been seen on one machine, so `systemd/clipper-listen` probes
each socket with `--list-monitors` instead of trusting an environment value.

**EasyEffects is per-app, not global.** It publishes a virtual mic
(`easyeffects_source`); PipeWire's own default input stays the raw hardware mic,
so an app only gets the gate and noise suppression if it names that node.
`gsr::mic_source()` checks whether the node exists and uses it, falling back to
`default_input` when EasyEffects is not running — naming a node that is absent
records no microphone at all. The clipper's own `pw-record` deliberately listens
on the raw mic: a noise gate can swallow quiet speech, and a swallowed phrase is
a missed clip.

**Process discovery must read `/proc/*/cmdline`, not `/proc/*/comm`.** `comm` is
truncated to 15 characters, so `gpu-screen-recorder` appears as
`gpu-screen-reco` and never matches.

**egui 0.29 has no arc primitive**, so the power glyph is a sampled polyline.

**eframe never sets a Wayland app_id** (checked 0.29, 0.31 and 0.33: it uses
`app_id` only for its own storage path and never passes it to winit's
`platform_specific.name`). Plasma's task manager skips Wayland windows it cannot
associate with an application, so the window renders but never appears in the
taskbar. The desktop file runs the app with `WAYLAND_DISPLAY` unset, which makes
winit derive `WM_CLASS` from the executable name, and `StartupWMClass` matches
it. That is the only reason the GUI goes through XWayland.

**`cargo test --release` does not refresh `target/release/<bin>`.** It builds
the test harnesses, not the installable binaries, so `install` after a bare
`cargo test` silently ships the previous build. Always `cargo build --release`
before installing; this cost an hour of debugging a change that was correct.

## Mute design

`~/.local/state/clipper/muted` existing == muted. The listener stats that file
about twice a second. That means the GUI, `clipctl toggle`, and a keyboard
shortcut can all flip it without any IPC, and the listener still drains the mic
pipe while muted so PipeWire never backs up.

Every other toggle (keep-alive, microphone, desktop audio) uses the same trick
for the same reason: no IPC, no protocol, and the state survives a restart of
either process.

## Versioning

[Semantic versioning](https://semver.org/): `MAJOR.MINOR.PATCH`, and the number
in `Cargo.toml` is the single source of truth (`clipctl --version` reads it).
Tags are `v` + that number, and every tag gets a GitHub release.

The **public API** being versioned is what a user or script depends on:

- every `clipctl` subcommand and its flags
- the file formats listed above
- the environment variables listed above

| Bump | When | Example |
|---|---|---|
| **MAJOR** | Any of the above breaks. A subcommand is removed or renamed, a config file changes shape, a required env var appears. | Dropping OBS support entirely, which removed `clipctl init`'s OBS half and repointed `buffer` → 2.0.0 |
| **MINOR** | New capability, nothing existing changes. | New subcommand, new GUI control, new optional setting → 1.1.0 |
| **PATCH** | Bug fix only. No new surface, no behaviour change anyone has to learn. | A panic fix → 1.0.1 |

Two rules that are easy to get wrong:

1. **Versions only go up.** Never reuse or lower a number; a published tag is a
   promise.
2. **A fix bundled with a feature is a MINOR, not a PATCH.** The feature sets the
   bump.

## License

GPL-3.0-or-later. See `LICENSE`, and SPDX headers identify it in every source
file.

In plain terms:

- You may use it for anything, including commercially.
- You may fork it, modify it, and redistribute it.
- If you distribute a modified version, you must release its source under
  GPL-3.0 too, and say what you changed. That is the whole point: this code can
  be built on, but it can never be closed off.
- There is no warranty, as is standard for free software.

One thing people get wrong about GPL, worth stating out loud: **charging money
for copies is allowed.** What GPL forbids is making a derivative closed-source.
A fork sold for profit would have to ship its full source under the same terms,
which is what keeps the work open rather than what makes it free of charge.
