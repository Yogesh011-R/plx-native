# TVPlayer plan

TVPlayer is a fork of [PlxNative](https://github.com/GLinnik21/plx-native) (GPL-3.0-or-later).
This document says what the fork is for, what is done, and what comes next. Update it in the same
commit as the work it describes.

## Goal

A native video player for LG webOS TVs that plays files from a **USB drive** (and later the local
network) **without Plex**. It keeps what PlxNative does well:

- **Native, not a web app.** The UI is drawn on the GPU with SDL2 + OpenGL ES; no browser.
- **The TV's own hardware decoder.** Video and audio are decoded by LG's Starfish pipeline. The app
  only splits the file into compressed frames (FFmpeg's libavformat) and feeds them in.
- **Fast on old sets.** 60 fps UI on webOS 4.x-era TVs.

Non-goals for now: a Plex client, a media library with artwork and metadata, transcoding.

## How playback works (the part we reuse)

```
file / URL ──► ff.rs (bundled libavformat, custom AVIO source) ──► aq.rs (video + audio queues)
           ──► player/pump.rs (main thread, Feed() each frame) ──► src/starfish.c
           ──► LG Starfish decoder ──► ACB / exported window ──► TV video plane
```

Read `rust-modules/media/src/player/CLAUDE.md` before changing any of it. The no-Plex entry point
already exists: `install_synthetic_playurl` (a URL plus its codec declaration) followed by
`start_playback`, which is what the `/tmp/plxnative-playurl` dev trigger uses.

## Decisions

| Decision | Why |
|---|---|
| Bypass the Plex flow first, delete it later | Deleting it touches hundreds of files, and the player still uses Plex types (route, decision, plan). Normal boots root at `AppArg::Files`, so the Plex pages never run. |
| Controlled boots (record/replay harness) keep the Plex route | The committed replay fixtures were recorded on it. |
| Folder listing runs on a worker thread | A USB drive can take seconds to answer; the frame thread must not block. |
| Rows keyed by position | A folder listing has no other identity; every listing is a fresh `open`. |
| Media root: `PLXNATIVE_MEDIA_ROOT`, else `/tmp/usb`, `/media/usb`, `/mnt/usb`, else `~/Videos` | The TV paths are unverified guesses until tested on a set (milestone 3). |
| Keep the binary name `plxnative` and the `/tmp/plxnative-*` triggers | Internal only; renaming touches hundreds of places. |
| Telemetry stays compiled out | Credentials are only supplied by the upstream release workflow, which this fork does not run. |

## Milestones

Status: ✅ done · 🔄 next · ⬜ planned

### ✅ M0 — Fork and identity (commit `d5424c86`)
- App id `com.yogesh.tvplayer` (+ `.debug`, `.nightly`), title and Plex product name "TVPlayer".
- Placeholder logo, splash and icon sets (the PlxNative marks are not licensed to forks).
- README fork notice (GPL §5a). Fork on GitHub; publishing workflows disabled; CI builds the `.ipk`.

### ✅ M1 — USB file browser as the root page (commit `267ddba7`)
- `rust-modules/screens/src/files.rs`: folders first, then video files; OK opens a folder or emits
  `AppFx::PlayFile(path)`; BACK goes up, and at the root hands BACK to the TV.
- Registered as `AppArg::Files` (screen id 25, canon tag 13, route word `files`).
- `boot.rs`: non-controlled boots root at Files. `files.*` messages in en/es/be.
- `run.rs` drained `PlayFile` and only logged it (by extension, never by name); M2 plays it.
- Verified in the Linux simulator and by unit tests.

### 🔄 M2 — Play a local file (code done; awaiting a TV)
- `ff.rs`: a **file source** (`Src::File`) beside the http/https ones, chosen by
  `DemuxSource::File`; `probe_file` reads codecs, raster, frame rate (`AVStream.avg_frame_rate`,
  asserted in `ci/ffabi-assert.c`) and the Dolby Vision record.
- `player/local.rs`: `decide` turns the probe into the Load declaration or a `Refusal`
  (video not H.264/HEVC, no AC3/E-AC3/AAC/DTS audio track, no video, no audio, a Dolby Vision
  layering the set cannot show); `install` puts the declaration and a `file://` URL in the route.
  The engine sends `file://` URLs to the file source.
- `files.rs`: OK on a video probes it on a worker ("Opening…"); a playable file becomes
  `AppFx::PlayFile(LocalPlay)`, a refusal stays on the browser as a note. `run.rs` installs and
  calls `start_playback` from the browser, so BACK and end-of-file return there. Logs carry the
  extension only, never a file name.
- Known gaps: video-only files are refused (the payload needs an audio codec); the bundled
  FFmpeg has no AVI/MPEG-PS/WMV/FLV demuxer, so those list but refuse as unreadable; the player
  HUD shows no title for a local file.
- **Still to verify on a TV**: the 32-bit offset (CI's cross-build compiles the assertion), the
  Load, picture, sound, seeking and BACK. The Linux simulator shows only the probe, the
  declaration and the demuxer opening the file.

### ⬜ M3 — Confirm the TV side
- Find where webOS mounts USB storage and whether a Developer Mode app can read it (the jail).
- Fix `TV_ROOTS` in `files.rs` to match, and handle a drive plugged in or removed while open.

### ⬜ M4 — Remove the Plex flow in stages
1. UI entry points that can still reach Plex (Settings rows, account menu, Home/Library/Search).
2. Session/auth boot work that still runs (`boot: no session — starting QR sign-in` is logged).
3. The player's dependence on Plex route/decision types for local playback.
4. The Plex crates and their tests, once nothing names them.
5. Rewrite About and Privacy: they still name the original author and `support@plxnative.com`.
   **Required before sharing any build.**

### ⬜ M5 — Network sources
- Plain HTTP/HTTPS URLs (the transports already exist).
- DLNA/UPnP media servers (SSDP discovery + ContentDirectory browse; playback is HTTP).
- SMB shares (e.g. libsmb2 as another AVIO source).

### ⬜ M6 — Player features for local files
- Resume position per file, stored locally.
- External `.srt`/`.ass` subtitles next to the video (the renderers exist).
- Audio and subtitle track choice for embedded tracks.

### ⬜ M7 — Test and CI hygiene
- Re-record the three replay fixtures (`tools/plxnative-rec rerecord`); Simulator CI's macOS
  replay job fails until then (the Settings text rename broke one; the M1 shape change broke all).
- A focus-probe grammar for Files (it reads as Home today) and `tests/manifest.json` scenes.

### ⬜ M8 — Release
- Real name and artwork, rewritten About/Privacy, release `.ipk` and install instructions.

## Development setup

Building, installing and launching the `.ipk` on a TV, step by step:
[tvplayer-build-and-install.md](tvplayer-build-and-install.md).

- **TV build:** push to `main` on the fork; the `CI` workflow's `cross-build` job uploads
  `com.yogesh.tvplayer.debug_*_arm.ipk`. The webOS NDK has no x86_64 Linux build, so it cannot
  run locally on this machine. Install with webOS Dev Manager (Developer Mode).
- **Linux simulator** (UI only, no video; needs `sdl2_ttf`):
  - Build: `make -C ~/dev/tvplayer sim-linux SIM_TDIR=~/.cache/tvplayer/target-sim SIM_LINUX_TDIR_ENV=~/.cache/tvplayer/target-sim`
  - Run: `~/.cache/tvplayer/run-sim.sh`; set `PLXNATIVE_MEDIA_ROOT=<dir>` to browse a folder.
    A test tree is at `~/.cache/tvplayer/test-media`.
  - Keys: arrows, Enter = OK, Esc/`q` = Back. The mouse acts as the Magic Remote pointer.
  - Log: `~/.local/state/tvplayer-sim/plxnative-events.log`.
  - To exercise local-file probing and demuxing, the simulator needs FFmpeg 9 (libavformat 63)
    in its app dir under the `-plx` names. Building it (`HOST=1 ci/build-ffmpeg.sh`) needs
    `nasm`; a system FFmpeg of the same majors can stand in by symlinking
    `libavutil-plx.so.61`, `libavcodec-plx.so.63` and `libavformat-plx.so.63` to it. The
    system build has more demuxers than the bundled one, so refusals can differ.
- **Tests:** `cargo test --lib -p plx_screens` and `-p plxnative-modules` (also with
  `--features plxnative-modules/hostsim`), `python3 ci/check-localization.py`,
  `bash ci/check-deps.sh`. Point `CARGO_TARGET_DIR` at a Linux filesystem.
- **No TV yet.** Options: LG webOS Cloud Test Lab (remote real sets; see `docs/lab-diagnostics.md`
  for the log bridge) or any LG TV with webOS 4.0+.
- The checkout lives on an NTFS drive (`~/dev`); keep build output on a Linux filesystem.
- Upstream fixes: `git pull upstream main`.
