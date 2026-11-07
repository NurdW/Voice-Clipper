# Voice Clipper — the how-to

Your PC records the last few minutes of gameplay into RAM all the time, and
saves it to a file when you say a phrase. Nothing is uploaded anywhere — the
speech recognition runs on your own machine.

Clips land in: **/mnt/storage/recordings-clips/GSR/GSR-clips/** (named
`Replay_2026-11-05_14-34-23.mp4` — that naming comes from GPU Screen Recorder,
not from us)

The recorder is **GPU Screen Recorder** (`gpu-screen-recorder`). It encodes on
your GPU instead of your CPU, so it costs almost no frames while you play.

---

## 1. The short version (do this)

1. Make sure the recorder is running. Click the **GSR** clapperboard once — it
   turns green. Tick **keep GSR buffer on** in its ▼ menu and it starts by
   itself at login and gets restarted if it ever dies.
2. Make sure the power button says **Clipper Enabled** (green).
   - The green button window = click it once to turn listening off/on.
   - Or press **Meta + Alt + C** (Windows key + Alt + C).
   - Or run `clipctl toggle` in a terminal.
3. Play your game. When something good happens, **say your phrase** — every
   word, clearly:
   - `chat clip that`
   - `chat did you see that`
   - `chat take notes`
4. About a second later you hear a short **ding**, and the last few minutes are
   written to a file. Check it: `xdg-open /mnt/storage/recordings-clips/GSR/GSR-clips`

That's it. There is no "start recording" step — it is always recording into RAM.

---

## 2. The window

| Control | What it does |
|---|---|
| **power button** | green = listening, grey = muted |
| **GSR** (clapperboard) | red = start the recorder; green = save a clip right now |
| **▼** next to it | opens the menu below |
| `stop GSR` | appears while it runs; stops recording and finalises the file |

The ▼ menu holds four things:

- **GSR clip length** — 30s, 1m, 2m, 3m or 5m
- **keep GSR buffer on** — the listener starts the recorder at login and
  restarts it within about 30 seconds if it dies
- **microphone** — off means your voice is not on the clip
- **game / desktop** — off means no game or app sound on the clip

Changing the clip length or the audio switches restarts the recorder, because
it picks its buffer size and its audio sources when it starts. The status line
tells you whether it restarted or will apply next time.

From a terminal, the length is the same thing:

```bash
clipctl buffer          # tell me the current length
clipctl buffer 30       # make every clip the last 30 seconds
```

---

## 3. Recording voice or not

Both audio switches are on by default, and they are independent:

| microphone | game / desktop | you get |
|---|---|---|
| on | on | everything, on separate tracks |
| on | off | your voice only |
| off | on | game and app sound, no voice |
| off | off | silent video |

This is not just a mute button. GPU Screen Recorder writes **each source as its
own track**, so even with everything on you can delete or lower your mic later
in an editor without touching the game audio. The switches are for when you know
you never want a source at all — a silent file is smaller and there is nothing to
forget to delete.

The sources themselves are listed in `~/.config/clipper/gsr_audio.txt`, one per
line:

```bash
gpu-screen-recorder --list-application-audio   # app names it can see right now
gpu-screen-recorder --list-audio-devices       # device names
```

An app that is **not running** when the recorder starts still gets a track, a
silent one, which keeps the track order stable between clips.

---

## 4. How long a clip is

A clip is exactly as long as the rolling buffer — set it in the ▼ menu or with
`clipctl buffer`. That is the whole answer for "I want a 30 second clip".

One buffer means one length: you cannot hold a 3-minute buffer and a 30-second
buffer at once. If you want a long buffer normally but a short clip now and then,
save as usual and cut the tail off the file:

```bash
shortclip                       # cuts the last 30s from the newest clip
shortclip 60                    # cuts the last 60s
shortclip 30 /mnt/storage/recordings-clips/GSR/GSR-clips/Replay_2026-11-05_14-34-23.mp4
```

The cut is a **stream copy** (no re-encoding), so it takes about a second and
does not lose quality. Output file gets `-short.mp4` added to the name.

The buffer lives in **RAM**, so length and bitrate are the same dial: 180 s at
40 Mbps is roughly 0.9 GB. If you would rather have the space back, lower one of
them.

### Why 40 Mbps and not more

Constant bitrate means every clip is the same size whether the scene was busy or
not, which is what keeps the RAM predictable. 40 Mbps at 1440p60 is about double
what YouTube targets when it re-encodes a 1440p60 upload (~20 Mbps), and that
headroom is worth paying for: a re-encode keeps whatever artefacts the source
already had, then spends bits re-encoding them. Beyond roughly twice their
target the extra bits are thrown away on upload, which is why this used to be
80 Mbps for no measurable gain. If you edit locally in DaVinci Resolve rather
than upload, the same reasoning applies — grading amplifies source artefacts.

---

## 5. Changing what you have to say

Edit this file, one phrase per line, lowercase:

```
~/.config/clipper/phrases.txt
```

Then restart the listener so it picks up the change:

```bash
systemctl --user restart clipper
```

**Every word must be heard exactly.** There is no forgiveness for a muffled
word: say "chat clip that" and Vosk must transcribe "chat clip that", all four
words. Words it doesn't know (made-up slang, some game names) get folded into
`[unk]` and will never match, so plain English words work best. Expect to
over-enunciate slightly at first; a missed trigger is the price of no phantom
clips.

### Test a phrase without guessing

Record yourself, then run it through the exact same recognizer the listener
uses. This is the honest way to find out if it understands *your* voice:

```bash
pw-record --rate 16000 --channels 1 /tmp/myvoice.wav   # say the phrase, then Ctrl-C
clipctl hear /tmp/myvoice.wav
```

You want to see `→ MATCH "your phrase"`. If it prints only part of the phrase,
say it a little slower, or add the shorter version to phrases.txt.

---

## 6. Turning it off (privacy)

Muting means: the mic keeps running but speech is ignored. Nothing is written
to disk unless a phrase matches, and nothing ever leaves the PC.

| Way | Command / action |
|---|---|
| Power button | click the green/grey window (it is called **voice clipper**) |
| Keyboard | Meta + Alt + C |
| Terminal | `clipctl mute` / `clipctl unmute` / `clipctl toggle` |
| Stop it completely | `systemctl --user stop clipper` |

`clipctl muted` prints which state you are in. Note that muting the *listener*
does not stop the recorder — it keeps recording into RAM and simply ignores
your voice. To stop recording entirely, click `stop GSR`.

---

## 7. Checking that everything is healthy

```bash
clipctl doctor      # checks the recorder, the model, the mic, the phrases
clipctl status      # recorder state, clip folder, audio switches, phrases
clipctl clip        # save a clip right now, no voice needed
journalctl --user -u clipper -f     # live log of what the listener heard
```

`clipctl doctor` output you want to see: every line `[ok  ]`.

---

## 8. If a voice phrase does nothing

Work through these in order:

1. `clipctl muted` — is it muted? Click the button.
2. `clipctl status` — does the recorder line say it is running? If not, click
   the **GSR** button, and tick **keep GSR buffer on** so this stops happening.
3. `clipctl clip` — if this saves a file, the recorder side is fine and the
   problem is the speech side. Go to section 5 and test with `clipctl hear`.
4. `journalctl --user -u clipper -n 30` — did it hear something at all?
   If it prints `heard: ...` with the wrong words, your phrase needs adjusting.
5. If the recorder will not start at all, its own error output is in
   `~/.local/state/clipper/gsr.log`. `reported no monitors` means the service
   could not see your Wayland session.

---

## 9. Transcribing audio with whisper

`pwcpp` is installed with the **medium** model at
`~/.local/share/clipper/models/ggml-medium.bin`.

```bash
transcribe recording.mp3            # prints the text
transcribe interview.wav -otxt      # also writes interview.wav.txt next to it
transcribe clip.mp4 -osrt           # subtitles file
```

Note `-otxt` writes `<yourfile>.wav.txt` (the input name plus `.txt`), not a
name you choose — that is how pwcpp works, so rename the file afterwards if you
want a nicer name.

Whisper is heavy (it used about 13 seconds for a 1-second clip on this machine),
so it is a "run it when you need it" tool, not something running in the
background. The voice clipping uses a much smaller model (Vosk) precisely because
that one can run all day while gaming.

---

## 10. What is installed, and where

| Thing | Where |
|---|---|
| The tools | `~/.local/bin/clipctl`, `~/.local/bin/voice-clipper`, `~/.local/bin/shortclip`, `~/.local/bin/transcribe` |
| Source code | `~/slopbot/general/clipper/` (Rust) — also [github.com/NurdW/Voice-Clipper](https://github.com/NurdW/Voice-Clipper) |
| Your phrases | `~/.config/clipper/phrases.txt` |
| Audio sources | `~/.config/clipper/gsr_audio.txt` |
| Clip folder | `~/.config/clipper/gsr_dir.txt` |
| Speech model (Vosk, 68 MB) | `~/.local/share/clipper/vosk-model-small-en-us-0.15/` |
| Whisper model (1.5 GB) | `~/.local/share/clipper/models/ggml-medium.bin` |
| Listener service | `systemctl --user {status,restart,stop} clipper` |
| Button autostart | `~/.config/autostart/voice-clipper.desktop` |
| Taskbar identity | `~/.local/share/applications/voice-clipper.desktop` |
| Hotkey | `~/.local/share/applications/clipper-toggle.desktop` + `kglobalshortcutsrc` |
| The recorder | `/usr/bin/gpu-screen-recorder`, from COPR `brycensranch/gpu-screen-recorder-git` |

### Settings you can set by environment

Put these in `~/.bashrc`, or in the service file if you want them permanent.

| Variable | Default | Meaning |
|---|---|---|
| `CLIPPER_GSR_MONITOR` | `auto` | which monitor; `auto` picks the first one gsr reports (`DP-1`, `DP-2`, `HDMI-A-1`) |
| `CLIPPER_GSR_SECONDS` | `300` | how far back the buffer reaches (the ▼ menu writes a state file instead) |
| `CLIPPER_GSR_DIR` | `gsr_dir.txt`, else `~/Videos/clips` | where replays are written |
| `CLIPPER_GSR_BITRATE` | `40000` | kilobits/sec, constant bitrate. This is a RAM dial too: 180 s at 40 Mbps is about 0.9 GB |
| `CLIPPER_GSR_FPS` | `60` | frame rate |
| `CLIPPER_GSR_AUDIO` | *(unset)* | overrides the whole audio file as one mixed track; rarely what you want |
| `VOICE_CLIPPER_POS` | *(unset)* | where the window opens, e.g. `3690,300` for a second monitor |

### Why the window runs through XWayland

eframe (the GUI toolkit, 0.29 through 0.33) never tells the window system an
application id, and Plasma's taskbar skips Wayland windows it cannot match to an
application — the window shows up on screen but never in the taskbar. Running it
with `WAYLAND_DISPLAY` unset makes it an X11 window, which gets a proper class
name (`voice-clipper`) that matches the desktop file, so it appears in the
taskbar with the right name. To go back to native Wayland, edit
`~/.config/autostart/voice-clipper.desktop` and remove `env -u WAYLAND_DISPLAY`.

### Undoing it all

```bash
systemctl --user disable --now clipper
rm ~/.config/systemd/user/clipper.service
rm ~/.config/autostart/voice-clipper.desktop
rm ~/.local/bin/clipctl ~/.local/bin/voice-clipper ~/.local/bin/shortclip ~/.local/bin/transcribe
rm ~/.local/share/applications/clipper-toggle.desktop ~/.local/share/applications/voice-clipper.desktop
dnf5 remove gpu-screen-recorder gpu-screen-recorder-gtk
dnf5 copr disable brycensranch/gpu-screen-recorder-git
```

Your clips and your `~/.config/clipper/` files are left alone; delete those by
hand if you want them gone.
