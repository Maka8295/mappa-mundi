# Mappa Mundi

An offline desktop app that shows where your photos were taken, like the
Places map in iPhone Photos. Point it at a folder of photos (full-resolution
HEIC, JPEG, Live Photos, videos) and it puts them on a world map, grouped into
thumbnail markers that split apart as you zoom in.

Nothing touches the network: the world map is compiled into the binary.

## Features

- **iPhone formats:** HEIC/HEIF (including the 12 MP tiled images iPhones
  write), JPEG, PNG, WebP, MOV/MP4.
- **Live Photos:** the still and its `.MOV` (or icloudpd's `_HEVC.MOV`) are
  paired by file name and shown as a single item with a **LIVE** badge. Press Space in the viewer to play the
  motion clip.
- **Videos with location:** placed using Apple's QuickTime location metadata
  (or the `©xyz`/`loci` boxes other cameras use).
- **Markers like Apple's:** grouped photos show the newest photo plus a
  count. Click one to see the group in a grid, or double-click to zoom in
  until it splits.
- **Viewer:** full resolution, with date, place name, coordinates and camera
  model. Navigate with ←/→, play Live Photos and videos with Space (silent;
  **Open…** plays with sound in your default player), and jump back with
  **Show on Map**.
- **Edits and iCloud metadata:** an edited version (`IMG_1234-adjusted.HEIC`,
  or a JPEG beside the HEIC) is shown in place of its original, as in Photos.
  XMP sidecars (`IMG_1234.HEIC.xmp`) override the file's location and date,
  so places you added or fixed in Photos are used. Their captions and
  favourites (♥) appear in the viewer.
- **Offline place names:** "Dublin, Ireland", "London, Paris & 2 more".
- **[dungeonOS](https://github.com/Maka8295/Dotfiles) look:** the
  stone-and-bronze palette, CloisterBlack headings and
  Fantasque Sans Mono text (from your installed fonts, with egui's defaults
  as a fallback), and the square double-ruled frame, on a dark stone map.
- **Colour:** iPhone photos are converted from Display P3 to sRGB so they
  don't look washed out.
- **Built for large libraries:** a parallel scan reads only the metadata,
  and results are cached so reopening is instant. Thumbnails come from the
  preview embedded in each HEIC where there is one, and are cached on disk.

## Building

Requirements:

- Rust 1.95 or newer (egui's minimum)
- `libheif` (1.17+) with its development files and `pkg-config`
  - Arch: `pacman -S libheif`
  - Debian/Ubuntu: `apt install libheif-dev pkg-config`
  - Fedora: `dnf install libheif-devel`
- `ffmpeg` at runtime, for video thumbnails and Live Photo playback. It's
  optional; photos work without it.

```sh
cargo build --release
./target/release/mappa-mundi ~/Pictures/iPhone
# or install it: cargo install --path .
```

You can also start it with no arguments and use **Open Folder…**. The folder
is remembered for next time.

## Controls

| Action | How |
| --- | --- |
| Pan | Drag (it glides when you let go) |
| Zoom | Scroll wheel, pinch, double-click, `+`/`−` keys or buttons |
| See the photos in a group | Click a marker |
| Zoom into a group | Double-click a marker |
| Open a photo | Click it in the side panel |
| Next / previous photo | `→` / `←` |
| Play Live Photo / video | Space, or the **LIVE** / **Play** button |
| Close viewer or panel | Esc |

## Getting photos off the iPhone with their location intact

Location lives in each file's metadata. Many transfer routes strip it, for
example sending through messaging apps or sharing as "Most Compatible" without
"All Photos Data". These routes keep it:

- **Linux:** connect the phone and copy `DCIM/` with a file manager (gvfs/afc)
  or `ifuse`, or use `gphoto2 --get-all-files`.
- **macOS:** Image Capture, or File → Export → Export Unmodified Original in
  Photos.
- **Windows:** the Photos app's import, or copy `DCIM` in Explorer.
- **iCloud:** use `tools/icloud-photos` (next section). It gets the full
  library, Live Photo clips and edits included.

On the phone, setting *Settings → Photos → Transfer to Mac or PC* to **Keep
Originals** keeps the files as HEIC. Live Photo clips (`IMG_1234.MOV` next to
`IMG_1234.HEIC`) come along automatically.

## Downloading your iCloud library

`tools/icloud-photos` downloads the whole library at full quality with
[icloudpd](https://github.com/icloud-photos-downloader/icloud_photos_downloader),
which uses the same private web interface as icloud.com. Apple has no public
Photos API. It saves:

- **Originals:** the original of every photo and video, byte-for-byte as
  the iPhone uploaded it: full-resolution HEIC with all its EXIF, including
  GPS.
- **Edits:** the edited version of every edited photo, next to its original.
- **Live Photo clips:** every clip at full quality, named like its photo
  (`IMG_1234_QVRmWmV.HEIC` + `IMG_1234_QVRmWmV.MOV`). The 7-character id
  keeps names unique, since IMG_ numbers repeat.
- **Sidecars:** an XMP sidecar per file with what only iCloud knows:
  locations and dates changed in Photos, captions, keywords, favourites.
- **Shared Library:** the iCloud Shared Library too, if you're in one, under
  `Shared Library/`.

One-time setup:

1. Install icloudpd: `paru -S icloudpd-bin`. It's the official release
   binary, packaged in the AUR.
2. On the iPhone, under *Settings → [your name] → iCloud*:
   - turn **Access iCloud Data on the Web** on;
   - turn **Advanced Data Protection** off. icloudpd can't read an
     ADP-encrypted library.
3. Sign in in a terminal. It asks for your Apple ID password and a 2FA code;
   the password isn't stored, and the session lasts about two months.

   ```sh
   tools/icloud-photos login            # library goes to ~/ssd/icloud-photos
   tools/icloud-photos login --dest ~/Pictures/icloud   # or somewhere else
   ```

Then:

```sh
tools/icloud-photos download   # stop with Ctrl-C any time; re-run to continue
tools/icloud-photos verify     # checks every file, prints counts to compare with the iPhone
tools/icloud-photos status     # login age, last run
mappa-mundi ~/ssd/icloud-photos
```

The wrapper also handles icloudpd 1.32.3's known problems:

- **Exit status:** icloudpd reports success even when it fails, so the
  wrapper reads its log instead.
- **Failed files:** icloudpd tries each file only once, and iCloud
  sometimes ends a listing early. The wrapper keeps running passes until one
  finds nothing new.
- **Hangs:** stalled runs are killed after 20 quiet minutes and restarted.
- **Sidecar crash:** icloudpd crashes on some edited photos while writing
  their sidecar (issue #1063), and would get stuck on the same photo on every
  run. Sidecars are therefore written in a separate pass that steps around
  any photo it crashes on, and lists them.
- **Truncated videos:** very large videos can be cut short (issue #1311).
  `verify` checks each HEIC/MOV/MP4's container structure, probes videos
  with ffprobe, and decodes the end of large ones.
- **Unmounted drive:** if the library's drive isn't mounted, `download`
  refuses to run, so it can't silently fill the disk underneath.

`--hidden` also downloads the Hidden album (into `Hidden/`). The naming options
are fixed on purpose: changing them later makes icloudpd download everything
again under new names. Logs are in `<library>/.icloud-photos-logs/`.

## Street-level offline maps (optional)

The built-in map comes from [Natural Earth](https://www.naturalearthdata.com/)
data (1:10m): coastlines, borders, states, rivers, lakes, major roads, urban
areas and city names. That's good down to roughly city level. For street
detail, point the app at raster tiles you have locally:

```sh
mappa-mundi ~/Pictures/iPhone --tiles ~/maps/region.mbtiles   # raster MBTiles (png/jpg/webp)
mappa-mundi ~/Pictures/iPhone --tiles ~/maps/tiles/           # folder laid out as z/x/y.png
```

The overlay is remembered, so you only pass `--tiles` once. Wherever it has no
tiles, the built-in map shows through, and past its maximum zoom it's scaled
up. QGIS can produce MBTiles from any map source you're licensed to use
(Processing → *Generate XYZ tiles (MBTiles)*). Don't bulk-download from
tile.openstreetmap.org; its usage policy forbids that. Vector (`pbf`) MBTiles
aren't supported.

## Where things are stored

- `~/.config/mappa-mundi/config.json`: the library folder, tile overlay and last
  map position.
- `~/.cache/mappa-mundi/index/`: the metadata cache, one file per library folder.
  Changed files are re-read automatically; press ⟳ to pick up new files.
- `~/.cache/mappa-mundi/thumbs/`: the thumbnail cache. Safe to delete.

Your photos are only ever read, never modified.

## Development

```sh
cargo test                      # unit tests
cargo test --release -- --ignored --nocapture   # opt-in tests below (need env vars)
```

- `MAPPA_MUNDI_SAMPLES=dir MAPPA_MUNDI_SAMPLES_OUT=out` decodes every file in `dir`
  (metadata, thumbnail and full size) and writes PNGs to `out`. Use it to
  check orientation and colour when a photo looks wrong.
- `MAPPA_MUNDI_SCAN_DIR=dir` times a cold scan and a cached rescan.
- `cluster_200k` times clustering 200,000 photos (about 0.7 s).

### Rebuilding the built-in map

`assets/basemap.bin` is generated from Natural Earth GeoJSON:

```sh
mkdir -p ne-data && cd ne-data
base=https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson
for f in ne_110m_land ne_50m_land ne_10m_land ne_10m_minor_islands ne_50m_lakes ne_10m_lakes \
         ne_110m_admin_0_boundary_lines_land ne_50m_admin_0_boundary_lines_land \
         ne_10m_admin_0_boundary_lines_land ne_50m_admin_1_states_provinces_lines \
         ne_10m_admin_1_states_provinces_lines ne_50m_rivers_lake_centerlines \
         ne_10m_rivers_lake_centerlines ne_10m_roads ne_10m_urban_areas \
         ne_10m_populated_places_simple ne_50m_admin_0_countries; do
  curl -sfLO "$base/$f.geojson"
done
cd .. && cargo run --release --example build_basemap -- ne-data assets/basemap.bin
```

### Layout

| File | Purpose |
| --- | --- |
| `src/app.rs` | Window, markers, side panel |
| `src/map.rs` | Map view, input, tile drawing, labels |
| `src/basemap.rs` | Decodes and rasterises the embedded world map |
| `src/tiles.rs` | Tile layers (built-in, MBTiles, tile folder) |
| `src/library.rs` | Folder scan, EXIF/GPS, grouping originals with edits and Live clips, index cache |
| `src/xmp.rs` | XMP sidecars (icloudpd, Apple Photos, Lightroom) |
| `src/theme.rs` | dungeonOS palette, fonts and frames |
| `src/quicktime.rs` | Location and date from MOV/MP4 |
| `src/imageio.rs` | HEIC/JPEG decoding, orientation, P3→sRGB, thumbnail cache |
| `src/video.rs` | ffmpeg poster frames and playback |
| `src/cluster.rs` | Zoom-level marker grouping |
| `src/viewer.rs` | Full-screen viewer |
| `tools/build_basemap.rs` | Natural Earth → `assets/basemap.bin` |
| `tools/icloud-photos` | iCloud download wrapper (Python, stdlib only) |

Map data: Natural Earth (public domain).
