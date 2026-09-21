import { test, expect } from '@playwright/test';

test('landing page is usable and leads to installation', async ({ page }, testInfo) => {
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.goto('/');
  await expect(page.getByRole('heading', { level: 1 })).toHaveText(
    'Your money.Your books. Your machine.',
  );
  await expect(page.locator('body')).toBeVisible();
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
  ).toBeTruthy();
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({
    path: testInfo.outputPath('home.png'),
    fullPage: true,
    animations: 'disabled',
  });
  await page.getByRole('button', { name: 'Copy install command' }).click();
  await expect(page.locator('#copy-status')).toContainText(/Copied|Select and copy/);
  await page.getByRole('link', { name: 'Start your ledger' }).click();
  await expect(
    page.getByRole('heading', { name: 'Installation & first run', exact: true }),
  ).toBeVisible();
  expect(errors).toEqual([]);
});

test('documentation navigation and production search work', async ({
  page,
  isMobile,
}, testInfo) => {
  await page.goto('/docs/getting-started/');
  expect(
    await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
  ).toBeTruthy();
  await page.screenshot({ path: testInfo.outputPath('docs.png'), fullPage: true });
  await page.getByRole('button', { name: 'Search', exact: false }).first().click();
  const search = page.locator('.pagefind-ui__search-input');
  await search.fill('reconciliation');
  await expect(page.locator('.pagefind-ui__result-link').first()).toBeVisible();
  await page.keyboard.press('Escape');
  if (isMobile) await page.getByRole('button', { name: 'Menu', exact: true }).click();
  await page.getByRole('link', { name: 'Core concepts', exact: true }).first().click();
  await expect(page.getByRole('heading', { level: 1 })).toHaveText('Core concepts');
});

test('real TUI recordings autoplay silently without controls', async ({ page }) => {
  await page.goto('/');
  const videos = page.locator('video');
  await expect(videos).toHaveCount(2);
  for (const video of await videos.all()) {
    await video.scrollIntoViewIfNeeded();
    await expect
      .poll(() => video.evaluate((v: HTMLVideoElement) => v.readyState))
      .toBeGreaterThanOrEqual(2);
    const metadata = await video.evaluate((v: HTMLVideoElement) => ({
      duration: v.duration,
      width: v.videoWidth,
      height: v.videoHeight,
      tracks: v.textTracks.length,
      error: v.error,
    }));
    expect(metadata.error).toBeNull();
    expect(metadata.duration).toBeGreaterThan(20);
    expect(metadata.width).toBe(1248);
    expect(metadata.height).toBe(714);
    expect(metadata.tracks).toBe(1);
    expect(
      await video.evaluate((v: HTMLVideoElement) => ({
        autoplay: v.autoplay,
        muted: v.muted,
        loop: v.loop,
        controls: v.controls,
      })),
    ).toEqual({ autoplay: true, muted: true, loop: true, controls: false });
    await expect
      .poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime))
      .toBeGreaterThan(0.1);
    await video.evaluate((v: HTMLVideoElement) => {
      v.pause();
      v.currentTime = 18;
    });
    await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.seeking)).toBe(false);
    expect(await video.evaluate((v: HTMLVideoElement) => v.error)).toBeNull();
  }
});

test('reduced motion pauses recordings, including preference changes', async ({ page }) => {
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await page.goto('/');
  const video = page.locator('video').first();
  await video.scrollIntoViewIfNeeded();
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.paused)).toBe(true);
  expect(await video.evaluate((v: HTMLVideoElement) => v.autoplay)).toBe(false);
  const button = page.locator('.demo-player').first().getByRole('button');
  await expect(button).toHaveText('Play demo');
  await button.click();
  await expect
    .poll(() => video.evaluate((v: HTMLVideoElement) => v.currentTime))
    .toBeGreaterThan(0.1);
  await expect(button).toHaveText('Pause demo');
  await button.click();
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.paused)).toBe(true);
  await page.emulateMedia({ reducedMotion: 'no-preference' });
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.paused)).toBe(false);
  await page.emulateMedia({ reducedMotion: 'reduce' });
  await expect.poll(() => video.evaluate((v: HTMLVideoElement) => v.paused)).toBe(true);
  await expect(
    page.getByRole('link', { name: /Open video|Download terminal recording/ }),
  ).toHaveCount(0);
});

test('blocked autoplay offers a working play action', async ({ page }) => {
  await page.addInitScript(() => {
    // Block native autoplay too; mocking play() alone leaves it running.
    const autoplay = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'autoplay')!;
    Object.defineProperty(HTMLMediaElement.prototype, 'autoplay', {
      ...autoplay,
      set() {
        autoplay.set!.call(this, false);
      },
    });
    let clicked = false;
    document.addEventListener(
      'click',
      (event) => {
        if (event.isTrusted && (event.target as Element).closest('.demo-play')) clicked = true;
      },
      true,
    );
    const original = HTMLMediaElement.prototype.play;
    HTMLMediaElement.prototype.play = function () {
      if (!clicked) return Promise.reject(new DOMException('Autoplay blocked', 'NotAllowedError'));
      return original.call(this);
    };
  });
  await page.goto('/');
  const player = page.locator('.demo-player').first();
  await player.scrollIntoViewIfNeeded();
  const button = player.getByRole('button', { name: 'Play demo' });
  await expect(button).toBeVisible();
  await button.click();
  await expect
    .poll(() => player.locator('video').evaluate((v: HTMLVideoElement) => v.currentTime))
    .toBeGreaterThan(0.1);
  await expect(button).toBeHidden();
});
