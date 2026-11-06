# clipper

Voice-triggered game clipping on Linux, built on OBS. Say `chat clip that` and
the last few minutes of gameplay get written to disk. Fully offline: no cloud,
no account, no Windows.

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
                                              obs-websocket (localhost:4455)
                                                                    │
                                                                  OBS ──▶ /mnt/storage/obs-record/Clip_*.mp4
```

## Layout

| Path | What |
|---|---|
| `src/main.rs` | `clipctl` CLI: clip / status / mute / unmute / toggle / listen / hear / init / doctor / gsr |
| `src/lib.rs` | library target so the GUI and CLI share one implementation |
| `src/obs.rs` | obs-websocket 5.7.x client (auth, requests) |
| `src/vosk.rs` | hand-written FFI to `libvosk` (Fedora `vosk-api-devel`) |
| `src/mic.rs` | mic capture by piping `pw-record` (must pass `--raw`, see gotchas) |
| `src/sound.rs` | plays a short sound after a clip lands |
| `src/listen.rs` | the always-on loop, plus `hear` (offline phrase testing) |
| `src/config.rs` | paths, phrase file, OBS websocket settings, OBS output folder (parsed from its ini files) |
| `src/gsr.rs` | GPU Screen Recorder control (start / save / stop by signal, `/proc` discovery) |
| `src/state.rs` | the mute flag (a file's existence — deliberately dumb) |
| `gui/` | `voice-clipper`: power button, clip button, gsr button (eframe/egui, same toolkit as glaunch-gui) |
| `systemd/` | user service for the always-on listener |
| `desktop/` | autostart + taskbar identity file (`@HOME@` is substituted by install.sh) |
| `assets/` | app icon source (SVG) and the 64x64 RGBA embedded for the window icon |
| `tools/obs_probe.py` | throwaway probe used to verify OBS before the Rust client existed |

## Build and test

```bash
cargo build --release
cargo test --release          # grammar JSON, phrase matching, auth hashing, arc geometry
./install.sh                  # installs to ~/.local/bin, enables the user service
```

Requires: `vosk-api-devel` (libvosk + header), `pipewire-utils` (pw-record),
`ffmpeg` (for `clipctl hear`), and OBS with obs-websocket enabled.
The Vosk model is downloaded to `~/.local/share/clipper/`, not committed here.

## Things that are not obvious (learned the hard way)

**obs-websocket 5.7.x is not the protocol the docs describe.** Verified against
the 5.7.3 sources:

- The **server** sends `Hello` (op 0) first. If your client sends Hello, you get
  closed with `4007 non-Identify message while not identified`.
- Op codes: `Hello=0 Identify=1 Identified=2 Reidentify=3 Event=5 Request=6
  RequestResponse=7 RequestBatch=8 RequestBatchResponse=9`.
- rpcVersion 1 uses a **flat** envelope: `d.requestType`, `d.requestData`,
  `d.requestStatus`, `d.responseData` — not the older `d.data.opType` nesting.
- `requestStatus.result` is a **boolean** (code 100 = success), not the string
  `"Ok"`.
- Auth: `secret = b64(sha256(password + salt))`, then
  `auth = b64(sha256(secret + challenge))`. It is the **salt** in the first step.
- The replay-buffer request is `SaveReplayBuffer`, not the 4.x name
  `SaveRecordReplayBuffer`.

**Vosk grammar must be plain JSON.** `["clip that", "[unk]"]`. Escaping the
quotes makes the FST reader fail with `ERROR: Parse: Unknown starting character`.

**`vosk_set_log_level(0)` is not silent.** Kaldi still prints LOG lines at 0;
use `-1`.

**The endpointer default is too eager.** With defaults, a fast "clip that" came
back as just `clip`. `vosk_recognizer_set_endpointer_delays(rec, 5.0, 1.0, 20.0)`
fixed it.

**A signal is not a mic failure.** SIGTERM interrupts the blocking pipe read with
`EINTR`; retry on `ErrorKind::Interrupted` or shutdown looks like a crash.

**egui 0.29 has no arc primitive**, so the power glyph is a sampled polyline.

**eframe never sets a Wayland app_id** (checked 0.29, 0.31 and 0.33: it uses
`app_id` only for its own storage path and never passes it to winit's
`platform_specific.name`). Plasma's task manager skips Wayland windows it cannot
associate with an application, so the window renders but never appears in the
taskbar. The desktop file runs the app with `WAYLAND_DISPLAY` unset, which makes
winit derive `WM_CLASS` from the executable name, and `StartupWMClass` matches
it. That is the only reason the GUI goes through XWayland.

**`pw-record` to stdout needs `--raw`.** Without it pw-record writes a 24-byte
Sun AU header (`.snd`) to stdout, shifting the whole PCM stream so every sample
is read misaligned. Nothing errors: bytes flow, the process stays alive, and the
recognizer is fed noise forever. A file argument hides this because a `.raw`
extension implies raw mode — stdout does not. Diagnosed by reading the same
bytes at offset 0 (garbage) and offset 1 (clean).

**A flat mic reading is not proof of a muted mic.** An empty room and a dead
input look identical in a short capture window, so `clipctl doctor` reports
"quiet" instead of blaming hardware. On this codec the ALSA `Rear Mic [off]`
control is a monitor path, not the capture path, and proved nothing.

**In gsr replay mode `-o` is a directory, not a file.** Passing `-o dir/gsr.mp4`
makes it *create* a directory called `gsr.mp4` and write `Replay_<ts>.mp4`
inside it. Pass the directory.

**OBS does not always record the active profile name.** `global.ini` has no
`[Basic] Profile=` key on this install, so `obs_record_dir()` scans
`basic/profiles/*/basic.ini` and takes the first output folder that exists.
`CLIPPER_GSR_DIR` overrides it.

**The websocket password is never copied into this project.** It is read from
OBS's own `plugin_config/obs-websocket/config.json` at runtime, so rotating it
in OBS's UI needs no change here.

**EasyEffects is per-app, not global.** It publishes a virtual mic
(`easyeffects_source`); PipeWire's own default input stays the raw hardware mic,
so an app only gets the gate and noise suppression if it names that node. OBS
and CS2 do. `gsr::mic_source()` checks whether the node exists and uses it,
falling back to `default_input` when EasyEffects is not running — naming a node
that is absent records no microphone at all. The clipper's own `pw-record`
deliberately listens on the raw mic: a noise gate can swallow quiet speech, and
a swallowed phrase is a missed clip.

**`cargo test --release` does not refresh `target/release/<bin>`.** It builds
the test harnesses, not the installable binaries, so `install` after a bare
`cargo test` silently ships the previous build. Always `cargo build --release`
before installing; this cost an hour of debugging a change that was correct.

**gsr puts each `-a` on its own audio track; sources joined with `|` are mixed
into one.** Separate flags produce streams tagged by origin (`name=Applications:
discord`), which is what makes per-source levels possible in an editor. The man
page does not say this; it was confirmed with ffprobe. The list lives in
`~/.config/clipper/gsr_audio.txt`, one source per line. An app that is not
running still yields a silent track, so track order stays stable across clips.

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

## Mute design

`~/.local/state/clipper/muted` existing == muted. The listener stats that file
about twice a second. That means the GUI, `clipctl toggle`, and a keyboard
shortcut can all flip it without any IPC, and the listener still drains the mic
pipe while muted so PipeWire never backs up.

## Versioning

[Semantic versioning](https://semver.org/): `MAJOR.MINOR.PATCH`, and the number
in `Cargo.toml` is the single source of truth (`clipctl --version` reads it).
Tags are `v` + that number, and every tag gets a GitHub release.

The **public API** being versioned is what a user or script depends on:

- every `clipctl` subcommand and its flags
- the file formats: `phrases.txt`, `gsr_audio.txt`, the state files in
  `~/.local/state/clipper/`
- the environment variables listed in the user guide

| Bump | When | Example |
|---|---|---|
| **MAJOR** | Any of the above breaks. A subcommand is removed or renamed, a config file changes shape, a required env var appears. | `clipctl buffer` becomes `clipctl length` → 2.0.0 |
| **MINOR** | New capability, nothing existing changes. New subcommand, new GUI control, new optional setting. | GSR multitrack audio → 1.1.0 |
| **PATCH** | Bug fix only. No new surface, no behaviour change anyone has to learn. | the gsr.log panic fix → 1.0.1 |

Two rules that are easy to get wrong:

1. **Versions only go up.** Never reuse or lower a number; a published tag is a
   promise.
2. **A fix bundled with a feature is a MINOR, not a PATCH.** The feature sets the
   bump.

