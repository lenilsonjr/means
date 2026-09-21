# Real TUI recordings

These captures run the actual `means` binary. They use a new temporary ledger for each scene. The seed data is fictional and has a music-studio theme. No live ledger, bank connection, personal path, or provider credential is used.

The captures contain only the terminal viewport, with no added frame or captions. The landing page adds a subtle hover lift and amber glow, and loops playback with sound and controls disabled. Reduced-motion preferences pause autoplay and show a small Play/Pause demo button. A Play demo button also appears when the browser blocks autoplay. Playback pauses offscreen and resumes when the video returns. There is no soundtrack. No band artwork or lyrics are included.

## Reproduce

Install Rust, protoc, Python 3, Node.js 24, and FFmpeg with H.264 encoding. From the repository root:

```sh
cargo build -p means-server
cargo build -p means-core --example website_demo
python3 website/demo/record.py
cd website
npm ci
npx playwright install chromium
node demo/render.mjs
npm run build
```

The seed example refuses an existing database path. The recorder starts a loopback server with provider variables absent, sends timed keystrokes to a real PTY, and saves the actual ANSI output as asciicast v2 files. It stops its server and removes the temporary ledger after each scene.

The renderer feeds that output to xterm.js at the recorded times. Playwright captures screenshots and video. FFmpeg encodes silent H.264 MP4 with fast-start metadata. WebVTT tracks supply workflow captions.

Inspect the captures after any change to the TUI. Timing or menu order can change the result. Verify that the split preview shows 72.40 and 18.10, and that confirmation produces three balanced postings with the statement link.

## Published media

- `overview.mp4`: accounts, a ledger, sorting, entry details, expense classes, and vault switching.
- `review-split.mp4`: a real imported draft, two categories, preview, confirmation, and the posted entry.
- PNG files: actual terminal frames from these sessions.
- `.cast` files: raw terminal output for inspection or replay.
- `.vtt` files: descriptive captions for the silent videos.

The samples are original project demo data. The source and generated captures follow the repository's Apache-2.0 license. Bundled website fonts retain their own licenses.

## README animation

The README reuses the PNG captures and a compact looping GIF derived from the split
recording. From the repository root, regenerate the GIF with:

```sh
mkdir -p docs/media
ffmpeg -y -ss 2 -t 25 -i website/public/media/review-split.mp4 \
  -filter_complex '[0:v]setpts=PTS/1.35,fps=6,split[a][b];[a]palettegen=max_colors=64:stats_mode=diff[p];[b][p]paletteuse=dither=none:diff_mode=rectangle' \
  -loop 0 docs/media/review-split.gif
```

This keeps the full terminal resolution and plays the workflow at 1.35× speed.
