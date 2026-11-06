# Voice Clipper — the how-to

Your PC records the last few minutes of gameplay into RAM all the time, and
saves it to a file when you say a phrase. Nothing is uploaded anywhere — the
speech recognition runs on your own machine.

Clips land in: **/mnt/storage/obs-record/** (named `Clip_2026-11-03_06-46-46.mp4`)

---

## 1. The short version (do this)

1. Make sure **OBS is open**. That is the recorder. If OBS is closed, nothing records.
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
   written to a file. Check it: `xdg-open /mnt/storage/obs-record` (opens the
   folder).

That's it. There is no "start recording" step — it is always recording into RAM.

---

## 2. How long a clip is

A clip is exactly as long as the rolling buffer. Set it in the app: click the
**`buffer 3m`** pill next to the clip buttons and pick 30s, 1m, 2m, 3m or 5m.
That is the whole answer for "I want a 30 second clip".

From a terminal it is the same thing:

```bash
clipctl buffer          # tell me the current length
clipctl buffer 30       # make every clip the last 30 seconds
```

Both go through OBS's own websocket and restart the buffer for you, so it takes
effect immediately. (By hand: OBS → Settings → Output → Replay Buffer seconds,
then Stop and Start the Replay Buffer.)

### Trimming a clip you already saved

OBS has **one** replay buffer, so you cannot hold a 3-minute buffer and a
30-second buffer at once. If you want a long buffer normally but a short clip
now and then, save as usual and cut the tail off the file:

```bash
shortclip                       # cuts the last 30s from the newest clip
shortclip 60                    # cuts the last 60s
shortclip 30 /mnt/storage/obs-record/Clip_2026-11-03_06-46-46.mp4
```

The cut is a **stream copy** (no re-encoding), so it takes about a second and
does not lose quality. Output file gets `-short.mp4` added to the name.

---

## 3. Changing what you have to say

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

## 4. Turning it off (privacy)

Muting means: the mic keeps running but speech is ignored. Nothing is written
to disk unless a phrase matches, and nothing ever leaves the PC.

| Way | Command / action |
|---|---|
| Power button | click the green/grey window (it is called **voice clipper**) |
| Keyboard | Meta + Alt + C |
| Terminal | `clipctl mute` / `clipctl unmute` / `clipctl toggle` |
| Stop it completely | `systemctl --user stop clipper` |

`clipctl muted` prints which state you are in.

The **voice clipper** window has two buttons:

- the big **power button** — green means listening, grey means muted
- **OBS** (clapperboard) — saves a clip immediately, same as saying your phrase
- **GSR** (clapperboard) — GPU Screen Recorder; see section 9
- **keep OBS buffer on** — while ticked, the listener restarts OBS's replay
  buffer if it finds it stopped. Untick it if you turned the buffer off on
  purpose and want it left alone.
- **keep GSR buffer on** — greyed out; gsr is started on demand by its own button

---

## 5. Checking that everything is healthy

```bash
clipctl doctor      # checks OBS, the websocket, the model, the mic, the phrases
clipctl status      # what OBS reports right now
clipctl clip        # save a clip right now, no voice needed
journalctl --user -u clipper -f     # live log of what the listener heard
```

`clipctl doctor` output you want to see: every line `[ok  ]`.

---

## 6. If a voice phrase does nothing

Work through these in order:

1. `clipctl muted` — is it muted? Click the button.
2. Is **OBS running**? The recorder is OBS; clipper only tells it when to save.
3. `clipctl status` — does it say `replay buf : running`?
   If not: in OBS click **Start Replay Buffer**. (clipper also tries to start it
   by itself every 30 seconds, but only while OBS is open.)
4. `clipctl clip` — if this saves a file, the OBS side is fine and the problem
   is the speech side. Go to section 3 and test with `clipctl hear`.
5. `journalctl --user -u clipper -n 30` — did it hear something at all?
   If it prints `heard: ...` with the wrong words, your phrase needs adjusting.

---

## 7. Transcribing audio with whisper (the other thing you asked for)

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

## 8. What is installed, and where

| Thing | Where |
|---|---|
| The tools | `~/.local/bin/clipctl`, `~/.local/bin/voice-clipper`, `~/.local/bin/shortclip`, `~/.local/bin/transcribe` |
| Source code | `~/slopbot/general/clipper/` (Rust) |
| Your phrases | `~/.config/clipper/phrases.txt` |
| Speech model (Vosk, 68 MB) | `~/.local/share/clipper/vosk-model-small-en-us-0.15/` |
| Whisper model (1.5 GB) | `~/.local/share/clipper/models/ggml-medium.bin` |
| Listener service | `systemctl --user {status,restart,stop} clipper` |
| Button autostart | `~/.config/autostart/voice-clipper.desktop` |
| Taskbar identity | `~/.local/share/applications/voice-clipper.desktop` |
| Hotkey | `~/.local/share/applications/clipper-toggle.desktop` + `kglobalshortcutsrc` |
| OBS config backup (before I changed anything) | `~/.config/obs-studio/backups/pre-clipper-20261029-172128/` |
| GPU Screen Recorder (Plan B) | `/usr/bin/gpu-screen-recorder`, from COPR `brycensranch/gpu-screen-recorder-git` |

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
# OBS settings, if you want them back exactly as they were:
cp -a ~/.config/obs-studio/backups/pre-clipper-20261029-172128/basic ~/.config/obs-studio/
dnf5 remove gpu-screen-recorder gpu-screen-recorder-gtk
dnf5 copr disable brycensranch/gpu-screen-recorder-git
```

---

## 9. Plan B: GPU Screen Recorder (installed, and the button works)

OBS uses your **CPU** for encoding (x264), which costs frames in a demanding
60fps game. GPU Screen Recorder does the whole job on the GPU instead, so it
costs almost nothing. It is installed and verified working on this machine
(61fps capture, replay save tested).

### The easy way: the `gsr` button

| Button state | What it does when you click |
|---|---|
| `GSR` with a **red** clapper | starts the GPU replay buffer in the background |
| `GSR` with a **green** clapper | saves the last few minutes right now |
| `stop GSR` (appears while it runs) | stops recording and finalises the file |

The little **▼** next to each clip button opens that recorder's own menu: its
clip length and its `keep ... buffer on` switch. The two keep-alive switches are
mutually exclusive on purpose — two recorders grabbing the same screen is double
the overhead for one clip. Whichever one is armed is what your voice phrase
clips; with neither running, the phrase falls back to OBS.

GPU clips go to `/mnt/storage/obs-record/gsr/` and are named
`Replay_2026-11-05_14-34-23.mp4` (that naming comes from gsr, not from us).

With `keep GSR buffer on` ticked, GPU Screen Recorder runs from login and is
restarted within about 30 seconds if it ever dies or you kill it. If it is not
coming up, the reason is in the log:

```bash
journalctl --user -u clipper --since '-10 min' | grep -i gpu-screen
```

`reported no monitors` means the service could not see your Wayland session — the
startup wrapper resolves that now, but a stale copy of the wrapper after an
`install.sh` change is worth checking first.

### Each audio source is its own track

This is the reason to prefer GPU Screen Recorder for clips you intend to edit.
Every source listed in `~/.config/clipper/gsr_audio.txt` is written as a
**separate audio track**, tagged with where it came from:

```text
track 1  Devices: Easy Effects Source     <- your mic, filtered
track 2  Applications: cs2
track 3  Applications: deadlock
track 4  Applications: discord
track 5  Applications: firefox
track 6  Applications: spotify
```

So in an editor you can pull the game down and push your mic up, independently.
Edit that file to add or remove sources, then restart GSR:

```bash
gpu-screen-recorder --list-application-audio   # app names it can see right now
gpu-screen-recorder --list-audio-devices       # device names
```

Two things to know: an app that is **not running** when GSR starts still gets a
track, a silent one (keeps the track order stable); and sources written with a
`|` between them are **mixed into one track** instead — one line per source is
what keeps them apart.

### The terminal way

```bash
clipctl gsr status   # is it running, and where do its clips go
clipctl gsr start    # start the replay buffer
clipctl gsr save     # write the last few seconds to disk
clipctl gsr stop     # stop recording
```

Under the hood it is roughly `gpu-screen-recorder -w DP-1 -c mp4 -r 300 -s 0x0
-f 60 -k h264 -bm cbr -q 80000 -a easyeffects_source -a app:cs2 ... -o <folder>`
— native resolution, 60 fps, H.264 at 80 Mbps — and saving is just `kill
-USR1` to its process. No socket, no extra daemon.

### Changing its settings

The audio list lives in `~/.config/clipper/gsr_audio.txt`. The rest are
environment variables, so put them in `~/.bashrc` or in the service file if you
want them permanent:

| Variable | Default | Meaning |
|---|---|---|
| `CLIPPER_GSR_MONITOR` | `auto` | which monitor; `auto` picks the first one gsr reports (`DP-1`, `DP-2`, `HDMI-A-1`) |
| `CLIPPER_GSR_SECONDS` | `300` | how far back the buffer reaches (the GSR ▼ menu writes a state file instead) |
| `CLIPPER_GSR_DIR` | your OBS clip folder + `/gsr` | where replays are written |
| `CLIPPER_GSR_BITRATE` | `80000` | kilobits/sec, constant bitrate. This is a RAM dial too: the buffer lives in RAM, so 300 s at 80 Mbps is about 3 GB |
| `CLIPPER_GSR_FPS` | `60` | frame rate |
| `CLIPPER_GSR_AUDIO` | *(unset)* | overrides the whole audio file as one mixed track; rarely what you want |


