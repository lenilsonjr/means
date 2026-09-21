/** Render recorded PTY output with xterm; never reconstruct the TUI from mock HTML. */
import { chromium } from '@playwright/test';
import { readFile, mkdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const root = fileURLToPath(new URL('../', import.meta.url));
const media = path.join(root, 'public/media');
const temporary = path.join(root, '.demo-render');
await mkdir(temporary, { recursive: true });
const xterm = await readFile(path.join(root, 'node_modules/@xterm/xterm/lib/xterm.js'), 'utf8');
const css = await readFile(path.join(root, 'node_modules/@xterm/xterm/css/xterm.css'), 'utf8');
const font = await readFile(
  path.join(
    root,
    'node_modules/@fontsource-variable/jetbrains-mono/files/jetbrains-mono-latin-wght-normal.woff2',
  ),
);
const scenarios = {
  overview: {
    screenshots: [
      [1.5, 'overview'],
      [4.5, 'account-ledger'],
      [13.5, 'expense-report'],
      [18, 'company-vault'],
    ],
    duration: 21.5,
  },
  'review-split': {
    screenshots: [
      [2.6, 'review'],
      [16, 'review-split'],
      [19, 'confirmation'],
      [26, 'posted-split'],
    ],
    duration: 27.5,
  },
};
const browser = await chromium.launch();
try {
  for (const [name, scene] of Object.entries(scenarios)) {
    const [header, ...events] = (await readFile(path.join(media, `${name}.cast`), 'utf8'))
      .trim()
      .split('\n')
      .map((line) => JSON.parse(line));
    const context = await browser.newContext({
      viewport: { width: 1440, height: 900 },
      deviceScaleFactor: 1,
      recordVideo: { dir: temporary, size: { width: 1440, height: 900 } },
    });
    const page = await context.newPage();
    await page.setContent(`<!doctype html><html><head><meta charset="utf-8"><style>${css}
      @font-face{font-family:DemoMono;src:url(data:font/woff2;base64,${font.toString('base64')}) format('woff2');font-weight:100 900}
      *{box-sizing:border-box}body{margin:0;background:#0c0b08;overflow:hidden}
      .xterm-viewport{overflow:hidden!important}.xterm{padding:0}
    </style></head><body><div id="terminal"></div><script>${xterm}</script></body></html>`);
    await page.evaluate(() => document.fonts.load('14px DemoMono'));
    await page.evaluate(
      ({ cols, rows }) => {
        window.term = new window.Terminal({
          cols,
          rows,
          fontFamily: 'DemoMono',
          fontSize: 13,
          lineHeight: 1,
          letterSpacing: 0,
          cursorBlink: false,
          disableStdin: true,
          allowProposedApi: true,
          theme: { background: '#0c0b08', foreground: '#f5b82e', cursor: '#edba66' },
        });
        window.term.open(document.getElementById('terminal'));
      },
      { cols: header.width, rows: header.height },
    );
    await page.evaluate(
      () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
    );
    const clip = await page.evaluate(() => {
      const { width, height } = document.querySelector('.xterm-screen').getBoundingClientRect();
      return { x: 0, y: 0, width: Math.ceil(width / 2) * 2, height: Math.ceil(height / 2) * 2 };
    });
    if (clip.width > 1440 || clip.height > 900) throw new Error('Terminal would be cropped.');
    let eventIndex = 0,
      screenshotIndex = 0;
    const started = Date.now();
    while ((Date.now() - started) / 1000 < scene.duration) {
      const elapsed = (Date.now() - started) / 1000;
      let output = '';
      while (eventIndex < events.length && events[eventIndex][0] <= elapsed) {
        output += events[eventIndex][2];
        eventIndex++;
      }
      if (output)
        await page.evaluate(
          (data) => new Promise((resolve) => window.term.write(data, resolve)),
          output,
        );
      if (
        screenshotIndex < scene.screenshots.length &&
        scene.screenshots[screenshotIndex][0] <= elapsed
      ) {
        await page.screenshot({
          clip,
          path: path.join(media, `${scene.screenshots[screenshotIndex][1]}.png`),
        });
        screenshotIndex++;
      }
      await new Promise((resolve) => setTimeout(resolve, 30));
    }
    const video = page.video();
    await context.close();
    const original = await video.path();
    const encode = spawnSync(
      'ffmpeg',
      [
        '-y',
        '-i',
        original,
        '-vf',
        `crop=${clip.width}:${clip.height}:0:0,fps=24`,
        '-c:v',
        'libx264',
        '-preset',
        'slow',
        '-crf',
        '21',
        '-pix_fmt',
        'yuv420p',
        '-movflags',
        '+faststart',
        '-an',
        path.join(media, `${name}.mp4`),
      ],
      { stdio: 'pipe' },
    );
    if (encode.status !== 0) throw new Error(encode.stderr.toString());
    console.log(`Rendered ${name}: screenshots and H.264 video (${clip.width}×${clip.height}).`);
  }
} finally {
  await browser.close();
}
