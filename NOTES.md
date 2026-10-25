# Mappa Mundi: bill of materials and development notes

Written 2026-10-25, at commit `6c78bc5` ("Play videos and Live Photos with
their sound"). The README covers features, building, controls and storage;
this file covers what the program is made of, how it works inside, why it
works that way, and how it's tested.

---

## 1. Summary

- Offline desktop app, written in Rust (edition 2024, Rust 1.95+), that puts
  a photo library on a world map and a timeline, with a full-screen viewer.
- About 5,900 lines of Rust in 21 modules, plus two Python tools
  (`tools/icloud-photos` and `tools/street-maps`).
- The app itself never uses the network. Only the two tools do.
- It's developed and tested on Linux (Hyprland on Wayland, PipeWire). It
  should build on macOS and Windows, but that hasn't been tried.
- Release binary: about 47 MB (thin LTO, not stripped). It embeds the 8.9 MB
  world map.

---

## 2. Bill of materials

### 2.1 Toolchain

| Item | Version / setting |
| --- | --- |
| Rust | 1.95 or newer (egui 0.37's minimum), edition 2024 |
| rustfmt | `max_width = 120`, `use_small_heuristics = "Max"` |
| Release profile | `lto = "thin"`; dependencies built at `opt-level = 2` in dev builds |
| Python (tools only) | Python 3, standard library only |

### 2.2 Direct Rust dependencies

| Crate | Version | Licence | Used for |
| --- | --- | --- | --- |
| eframe | 0.37.0 | MIT OR Apache-2.0 | Window, event loop, GPU renderer (wgpu; glow also compiled in) |
| egui | 0.37.0 | MIT OR Apache-2.0 | Immediate-mode UI |
| image | 0.25.10 | MIT OR Apache-2.0 | JPEG/PNG/WebP decoding, resizing (Lanczos3) |
| libheif-rs | 3.0.0 | MIT | HEIC/HEIF decoding (wraps the system libheif) |
| kamadak-exif | 0.6.1 | BSD-2-Clause | EXIF: GPS, date, orientation, camera model |
| cpal | 0.18.2 (`pulseaudio` feature) | Apache-2.0 | Sound output for video and Live Photo playback |
| rusqlite | 0.41.0 (`bundled`) | MIT | Reading MBTiles raster tile files (SQLite) |
| tiny-skia | 0.12.0 | BSD-3-Clause | Rasterising the world map and street tiles |
| flate2 | 1.1.10 | MIT OR Apache-2.0 | zlib (embedded map) and gzip (PMTiles) |
| rayon | 1.12.0 | MIT OR Apache-2.0 | Parallel library scan |
| crossbeam-channel | 0.5.17 | MIT OR Apache-2.0 | Worker queues, the audio sample queue |
| serde / serde_json | 1.0.229 / 1.0.154 | MIT OR Apache-2.0 | Config, index cache, ffprobe output |
| rfd | 0.17.2 | MIT | "Open Folder…" dialog (XDG desktop portal, with a zenity fallback) |
| walkdir | 2.5.0 | Unlicense OR MIT | Recursive folder scan |
| dirs | 7.0.0 | MIT OR Apache-2.0 | Config, cache and data folders |
| chrono | 0.4.45 | MIT OR Apache-2.0 | Dates and times |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | Error handling |

### 2.3 Transitive dependencies and licences

- **Counts:** 380 crates are compiled for Linux (`Cargo.lock` lists 537; the
  rest are for other platforms).
- **Licences:** almost all are MIT and/or Apache-2.0. The rest are BSD,
  Zlib, ISC, Unlicense, CC0 and Unicode-3.0, all permissive. The few worth
  knowing about:
  - **option-ext** 0.2.0 (via dirs): MPL-2.0. That's file-level copyleft,
    which only matters if that crate's own files are modified. Using it as is
    puts no conditions on this project.
  - **self_cell** 1.3.0: Apache-2.0 OR GPL-2.0-only. Use it under Apache-2.0.
  - **epaint_default_fonts** 0.37.0 (egui's built-in fonts): also under the
    OFL-1.1 and Ubuntu Font Licence, because it bundles those fonts.
  - **Unicode-3.0** (18 ICU and Unicode data crates): permissive, but needs a
    notice when redistributed.
- **Regenerating the counts:** `cargo tree -e normal,build --prefix none
  --target x86_64-unknown-linux-gnu` and `cargo metadata`. A formal SPDX or
  CycloneDX SBOM can be made with `cargo cyclonedx` or `cargo sbom` (not
  installed).

### 2.4 Native libraries (linked at build time)

| Library | How | Needed for |
| --- | --- | --- |
| libheif 1.17+ | System library, found with pkg-config (libheif-sys 5.3.1) | HEIC/HEIF photos |
| ALSA (alsa-lib) | System library, found with pkg-config (alsa-sys 0.4.0); always built on Linux | cpal's ALSA backend |
| SQLite | Compiled into the binary (libsqlite3-sys, bundled); public domain | MBTiles |
| GPU drivers | Vulkan, or GL through glow, loaded at run time by wgpu | Drawing |
| Wayland / X11 | winit (pure-Rust clients; X11 through x11rb) | Windowing |

The PulseAudio client (the `pulseaudio` 0.3.2 crate) and D-Bus (zbus, used
for accessibility through AccessKit/AT-SPI) are pure Rust, so they need no
system libraries.

### 2.5 Programs used at run time (all optional)

| Program | Used for | Without it |
| --- | --- | --- |
| `ffmpeg` 5.1+ | Video poster frames; decoding the picture and sound for playback | Videos show without thumbnails and can't play in the app |
| `ffprobe` (comes with ffmpeg) | Finds the video and sound streams and the frame rate | ffmpeg picks the streams itself, and frame rate is assumed to be 30 fps |
| PulseAudio or PipeWire (pipewire-pulse) | Sound output (cpal talks to the server directly) | Falls back to ALSA; with no sound at all, video plays silently |
| `xdg-open` (`open` on macOS) | **Open…** in the viewer | That button does nothing |
| xdg-desktop-portal, or `zenity` | **Open Folder…** dialog | Pass the folder on the command line instead |

### 2.6 Fonts (from the system if installed)

| Font | Used for | If it's missing |
| --- | --- | --- |
| Cloister Black | Blackletter headings | Falls back to the body font |
| FantasqueSansM Nerd Font | Body text | egui's built-in fonts |
| Noto Sans CJK JP | Japanese, Chinese and Korean place and street names | Those characters don't render |

### 2.7 Files and data in the repository

| File | What it is | Source / licence |
| --- | --- | --- |
| `assets/basemap.bin` (8.9 MB) | Built-in world map: land, lakes, rivers, borders, roads, urban areas, places; compressed with zlib | Natural Earth 1:10m/50m/110m, public domain; built by `tools/build_basemap.rs` and embedded with `include_bytes!` |
| `assets/mappa-mundi.svg`, `.desktop` | App icon and desktop entry | Made for this project |
| `docs/screenshots/*.webp` | README screenshots (1500×920) | Taken from a curated subset of the author's own photos (see section 6.3) |
| `tools/icloud-photos` | iCloud download wrapper | This project; drives icloudpd |
| `tools/street-maps` | Street-map downloader | This project |
| `tools/build_basemap.rs` | Natural Earth to `basemap.bin` | This project; a cargo example, so `cargo install` leaves it out |

### 2.8 External data and services (tools only, not the app)

| Service | Used by | Terms |
| --- | --- | --- |
| Protomaps daily planet build (`build.protomaps.com`), read with HTTP range requests | `tools/street-maps` | OpenStreetMap data, ODbL. The app shows "© OpenStreetMap" on the map |
| Apple iCloud Photos | `tools/icloud-photos`, through icloudpd 1.32.3 (installed separately, e.g. `paru -S icloudpd-bin`) | Your own account. The Apple password is never stored; icloudpd keeps a session cookie |
| Natural Earth GeoJSON (GitHub: nvkelso/natural-earth-vector) | Only when rebuilding `basemap.bin` | Public domain |

### 2.9 The project's own licence

MIT (`LICENSE`; also `license = "MIT"` in `Cargo.toml`). It's compatible
with every dependency above. Redistributed binaries should carry the
third-party notices: `cargo about` can generate them.

---

## 3. Architecture

### 3.1 Modules

| Module | Lines | Role |
| --- | --- | --- |
| `main.rs` | 70 | Command line, window options |
| `app.rs` | 838 | Top bar (Map / Timeline), markers, side panel, background scan, config |
| `map.rs` | 507 | Slippy-map view, input, tile drawing, labels |
| `basemap.rs` | 466 | Decodes and rasterises the embedded world map |
| `tiles.rs` | 225 | Tile layers: built-in, PMTiles streets, MBTiles, z/x/y folders |
| `pmtiles.rs` | 179 | PMTiles v3 reader |
| `mvt.rs` | 260 | Mapbox Vector Tile (protobuf) decoder |
| `streets.rs` | 372 | Street-map style; draws OpenStreetMap tiles and collects names |
| `geo.rs` | 44 | Web Mercator helpers (world coordinates in [0,1]²) |
| `cluster.rs` | 183 | Zoom-dependent grouping of photos into markers |
| `library.rs` | 597 | Folder scan, EXIF/GPS, pairing Live clips and edits, index cache |
| `xmp.rs` | 144 | XMP sidecars (icloudpd, Apple Photos, Lightroom) |
| `quicktime.rs` | 260 | Location and date from MOV/MP4 |
| `imageio.rs` | 248 | HEIC/JPEG decoding, orientation, Display P3 to sRGB, thumbnail disk cache |
| `media.rs` | 134 | Thumbnail, full-size and zoom texture caches |
| `texcache.rs` | 109 | GPU texture cache fed by workers, with LRU eviction |
| `work.rs` | 93 | Prioritised job queue drained by a worker pool |
| `viewer.rs` | 527 | Full-screen viewer: zoom, playback controls |
| `timeline.rs` | 348 | Photos by date |
| `video.rs` | 508 | ffmpeg poster frames; playback with sound and A/V sync |
| `theme.rs` | 156 | dungeonOS palette, fonts, frames |

### 3.2 Data flow

1. **Scan.** `library.rs` walks the folder in parallel with rayon, reading
   metadata only. Results are cached in
   `~/.cache/mappa-mundi/index/<hash>.json` and re-read only for changed
   files.
2. **Grouping.** Originals, edits (`-adjusted`, or a JPEG beside a HEIC),
   Live Photo clips (`IMG_N.MOV`, `IMG_N_HEVC.MOV`, icloudpd's `name-id7`
   names) and XMP sidecars are combined into one `Item` per photo or video.
   Sidecar locations and dates win over the file's own, as in Photos.
3. **Order.** `Library.items` holds the geotagged items first
   (`items[..located]`), then the ones without a location, whose
   coordinates are NaN (`Item::has_location()`). Each part is newest first.
4. **Views.**
   - The map and its clusters use only `items[..located]`.
   - The timeline uses every item.
   - The viewer takes a list of item indices: a cluster's members, or the
     timeline's order.
5. **Images.**
   - `media.rs` asks the worker pools (`work.rs`) for thumbnails (cache of
     800), fit-size images (2 workers, 8 kept) and zoom versions (2 workers,
     4 kept), by priority.
   - Each texture is keyed by `(item key, size)`.
   - Thumbnails come from the preview embedded in each HEIC where possible,
     and are cached on disk in `~/.cache/mappa-mundi/thumbs/`.

### 3.3 Threads

- **UI thread:** egui/eframe.
- **Scan:** rayon pool.
- **Images:** worker pools for thumbnails, full-size images and tile
  rasterising.
- **Each playback:**
  - `player` (reads the picture, paces the frames);
  - `player-sound` (feeds the decoded sound);
  - a stderr reader for frame times;
  - cpal's own threads (the PulseAudio driver and its latency poller).

---

## 4. Design decisions and why

### 4.1 Sharp photos in the viewer

- **No mipmaps.** egui's managed textures have no mipmaps, so a texture
  shrunk more than 2× on screen loses detail and shimmers.
- **Exact size.** Each photo is therefore decoded at exactly the size it's
  shown (`decode_edge`), resized with Lanczos3, and drawn 1:1, snapped to
  the pixel grid.
- **Exact means exact.** An earlier version rounded sizes up to 32 px; the
  1.7% shrink plus a quarter-pixel offset made it measurably softer.
- **Zoom versions.** Zooming loads a half-resolution version, then the
  original, capped at the GPU's `max_texture_side`.
- **Choosing a version.** `closest()` picks the smallest version at least
  0.95× the on-screen width.

### 4.2 Timeline

- **Layout.** Days with few photos share a line; longer days take full lines.
- **Scrolling.** `ScrollArea::show_viewport` lays out only the visible rows.
- **Headings.** Month headings stick to the top while scrolling.
- **Place names.** Place names under each day are worked out lazily, from a
  sample of 40 photos.
- **Year rail.** The year column on the right jumps to that year.

### 4.3 Video and Live Photo playback with sound

- **Two ffmpeg processes.** One decodes the picture as raw RGBA, scaled to
  the display size (at most 1280 px on the long edge), and one decodes the
  sound as 32-bit float PCM at the output device's rate and channel count.
  With two processes, each pipe applies its own back-pressure, so a full
  pipe can't stall the other stream.
- **The sound is the clock.** Each cpal callback records when its samples
  will be heard. That's `playback − callback` from `OutputCallbackInfo`,
  which on PulseAudio comes from the server's reported latency. The clock
  interpolates from there and never runs past the sound handed over so far.
- **Frames shown on time.**
  - Each frame is shown when the clock reaches its timestamp. Frames that
    are already late are skipped.
  - The sound waits for the first frame, so both start together.
- **Exact frame times.**
  - Live Photo clips have uneven frame rates (around 22–30 fps, with gaps).
  - The `rawvideo` muxer defaults to constant frame rate, which duplicates
    and drops frames.
  - So the picture runs with `-fps_mode passthrough` (hence ffmpeg 5.1+;
    ffmpeg 9 removed `-vsync`).
  - Each frame's time is read from the `showinfo` filter's log lines.
- **Slow motion.** 120 and 240 fps clips get `fps=60` to save work.
- **Buffer size.** cpal's PulseAudio backend defaults to a server-sized
  buffer of about 2 s. That would get padded with silence, so a fixed
  1024-frame buffer (about 21 ms) is requested.
- **Fallbacks.**
  - No sound stream, or no sound server: the picture runs on the wall
    clock.
  - The sound fails to start or stalls for 3 s: playback carries on without
    it.
  - The sound ends early: the clock continues on wall time from where the
    sound stopped.
- **Mute.** One flag for the whole session, applied as a gain in the
  callback, so it takes effect immediately and the clock keeps running.
- **Stopping.** Dropping the `Player` kills both ffmpeg processes
  (registered under a lock, so none can slip through), closes the sound
  stream and ends every thread.

### 4.4 Street maps

- **Partial download.** `tools/street-maps` reads only the PMTiles
  directory and the tiles within 3 km of each photo (configurable) from the
  140 GB planet file, using HTTP range requests.
- **Resumable.** Downloads can be stopped and resumed.
- **Zoom.** The data stops at zoom 15; the app keeps zooming to 20 by
  drawing the same vectors larger.

### 4.5 iCloud download tool

- **Settings.** It uses icloudpd's settings that keep the most:
  `--size original --size adjusted --live-photo-size original
  --live-photo-mov-filename-policy original --file-match-policy name-id7`,
  plus XMP sidecars.
- **`--parallel N`.**
  - A planning pass splits the library into date windows, and one icloudpd
    worker runs per window, each with its own copy of the session cookie.
  - A final sequential pass catches anything missed, then `verify` runs.
- **Lock.** An fcntl lock (`.icloud-photos.lock`) stops two runs at once.
- **Verify.** It checks each file by its content, not its extension:
  - **JPEG:** the end marker is present.
  - **WebP:** the RIFF size matches the file size.
  - **HEIC/MOV:** the ISO-BMFF box structure is complete.

  Decoders happily show a truncated HEIC, so decoding alone isn't enough.
  Some iCloud "JPG" files are really WebP.

---

## 5. Testing

### 5.1 Unit tests

- **Command:** `cargo test` runs 19 tests (map decoding, clustering, Live
  Photo and edit grouping, sidecars, QuickTime metadata, MVT, PMTiles, colour
  conversion, street styling).
- **Opt-in:** 3 tests are `#[ignore]`d and need environment variables. Run
  them with `cargo test --release -- --ignored --nocapture`:
  - `decode_samples` (`MAPPA_MUNDI_SAMPLES`, `MAPPA_MUNDI_SAMPLES_OUT`):
    decodes every file in a folder and writes PNGs.
  - `scan_timing` (`MAPPA_MUNDI_SCAN_DIR`): times a cold scan and a cached
    rescan.
  - `cluster_200k`: clusters 200,000 photos (about 0.7 s).
- **Before every commit:** `cargo fmt --check`,
  `cargo clippy --all-targets` (kept free of warnings) and `cargo test`.

### 5.2 Testing the GUI without disturbing the desktop (Hyprland)

1. **Hidden screen.** Create a virtual screen:
   `hyprctl output create headless PMTEST`, then
   `hyprctl eval 'hl.monitor({ output = "PMTEST", mode = "1920x1080@60", position = "6000x0", scale = 1 })'`.
2. **Launch.** Start the app there with
   `hyprctl eval 'hl.exec_cmd("…", { workspace = "<its workspace> silent", no_initial_focus = true, float = true, size = "1500 920", move = "40 20" })'`.
   - **`move` is relative to the screen.** An absolute position put the
     window off-screen. It then gets frames only now and then, and simulated
     input arrives late or not at all, which looks exactly like an app bug.
     Check the position with `hyprctl clients -j`.
3. **Isolated settings.** Use separate `XDG_CONFIG_HOME` and
   `XDG_CACHE_HOME`, so the real config and caches aren't touched.
4. **Simulated input.** A small "autopilot" module is inserted temporarily
   (never committed). It replays clicks, keys, scrolls and waits from an
   environment variable, and takes screenshots from inside the app with
   egui's `request_screenshot`. Don't use `grim`, which can capture other
   windows.
5. **Clean up.** `hyprctl output remove PMTEST`.

### 5.3 Testing sound without playing it on real speakers

1. **Private stack.** Run a separate PipeWire, WirePlumber and pipewire-pulse
   with their own runtime, config and state folders. WirePlumber uses a
   profile with no hardware access
   (`inherits = [ policy, mixin.systemwide-session, mixin.stateless ]`), and
   D-Bus is pointed nowhere.
2. **Virtual speaker.** Add a null sink and make it the default.
3. **Point the app at it.** Run the app with `PULSE_SERVER=unix:<private>/pulse/native` and
   `PIPEWIRE_REMOTE=<private>/pipewire-0`, so even the ALSA fallback can't
   reach the real server.
4. **Record.** `parec -d <sink>.monitor`, logging arrival times so the
   recording can be lined up with the app's frame times.
5. **Sync clip.** A test clip has a white flash and a 1 kHz beep at exactly
   1, 2, 3, 4 and 5 s, generated with ffmpeg `drawbox` and `aevalsrc`.

Results at `6c78bc5`:
- **Sync:** each flash frame was shown about 5 ms before its beep was
  heard.
- **Live Photo:** all 46 frames of a real clip played, each within
  0.3–2.1 ms of its sound.
- **Sound:** identical to the file's sound, sample for sample.
- **Mute:** silenced exactly the beeps in between.
- **Fallback:** with no sound server, frames ran on time to within 0.3 ms.
- **Clean-up:** after stopping, or switching photo mid-play, nothing was
  left behind (processes, threads or sound connections).
- **Not tested:** real Bluetooth headphones, where sync depends on PipeWire
  reporting the codec's latency correctly.

---

## 6. Working on it

### 6.1 Build, run, install

```sh
cargo build --release && ./target/release/mappa-mundi ~/Pictures/iPhone
cargo install --path .      # what the desktop entry runs (~/.cargo/bin/mappa-mundi)
```

### 6.2 Pushing

There's no CI. Check locally (section 5.1), then commit on `main` and push:

```sh
git -c credential.helper= -c 'credential.helper=!gh auth git-credential' \
    push https://github.com/Maka8295/mappa-mundi.git main
git update-ref refs/remotes/origin/main HEAD
```

### 6.3 Screenshots for the README (the repository is public)

- **Library.** Copy a curated set of photos (one trip) into a temporary
  folder. The scanner doesn't follow symlinks, so they must be real copies.
  Open the copy with isolated XDG folders, on the hidden screen at 1500×920.
- **Leave out:** home or other private locations, identifiable people, and
  anything that shows where the photographer stayed.
- **Convert:** `magick in.png -strip -quality 82 -define webp:method=6 out.webp`.
- **Afterwards:** delete the temporary copies.

### 6.4 Gotchas worth remembering

- **egui 0.37:**
  - `ui.id()` is deprecated; use `ui.scope_id()`.
  - The scroll state's `offset` is private; use `state.clamped_offset()`.
  - There is no `raw_scroll_delta`.
  - Don't call `ctx` methods inside a `ui.input(|i| …)` closure: it
    deadlocks.
- **egui multi-pass.** Input events are given only to the first pass of a
  frame, so key handling isn't run twice when a pass is discarded.
- **ffmpeg:**
  - The `null` muxer prints "non monotonically increasing dts" for many
    iPhone videos. It's harmless; filter it out when checking decodes.
  - ImageMagick's `-regard-warnings` fails on a missing EXIF field; query
    such fields separately.
- **File checks:** a truncated HEIC decodes without error, and a file's
  extension can lie. Check the structure and magic bytes.
- **Shell:**
  - zsh and fish don't word-split `$var`; use `bash -c` for scripts that rely
    on it.
  - `pkill -f pattern` from a shell whose own command line contains the
    pattern kills that shell. Use `pgrep -x`, or a script file.

---

## 7. Known limitations and ideas

- **Playback:**
  - Video is decoded at most 1280 px on the long edge, to keep the
    per-frame copies cheap.
  - There's no pause or seeking: playback runs once from the start. **Open…**
    hands the file to the desktop's player for that.
  - Mute isn't remembered after a restart.
  - Sync on Bluetooth headphones hasn't been checked (section 5.3).
- **Platforms:** only tested on Linux (Wayland, PipeWire). macOS and Windows
  builds are untested. The README's install steps are Linux-only.
- **Ideas:**
  - Show iCloud albums (Favourites and the like). icloudpd can list album
    membership, but the app doesn't read it.
  - Persist mute in the config.
  - Pause and seek during playback.
  - A generated third-party notices file and a formal SBOM for releases.
