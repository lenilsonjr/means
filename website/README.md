# means documentation website

A static Astro + Starlight site. The landing page has a custom design. Starlight supplies documentation navigation, local search, syntax highlighting, theme selection, and mobile navigation. Fonts are served with the site. No analytics or ledger API is used.

## Local development

Use Node.js 24 LTS (minimum 22.12).

```sh
cd website
npm ci
npm run dev
```

Open the address printed by Astro. Search uses the generated Pagefind index and is available in production preview, not the development server.

```sh
npm run check
npm run build
npm run preview
```

## Content

- `src/pages/index.astro`: landing page.
- `src/content/docs/docs/`: authored introductory pages.
- `scripts/guides.mjs`: explicit list of existing repository guides to publish.
- `scripts/prepare-docs.mjs`: copies those guides at build time, adds metadata, and resolves source links.
- `src/content/docs/docs/guides/`: generated and ignored. Do not edit this directory.
- `src/styles/`: landing-page and documentation themes.

Existing detailed guides stay in `../docs/`. Design proposals are not automatically published as product features. Relative links to unpublished files lead to their repository source.

## Validation

`npm run format:check` checks source formatting. `npm run build` checks internal page, asset, and fragment links after the static build.

The preparation step also includes license notices for the website components and bundled fonts. The generated `public/third-party-notices.txt` is shipped with the site.

```sh
npx playwright install chromium
npm test
```

Browser tests run against a production preview. They check desktop and mobile navigation, documentation search, copy feedback, and horizontal overflow. They save screenshots in the test artifacts.

## Deploy

The build output is `website/dist/`. Serve it with any static host. Use `website` as the project root and `npm run build` as the build command. The output directory is `dist`. This works with Vercel's static-site deployment; no server adapter or ledger process is needed.

Set `SITE_URL` to the public origin before a production build. Set `SITE_BASE` when the site lives under a subpath. For example:

```sh
SITE_URL=https://lenilsonjr.github.io SITE_BASE=/means npm run build
```

The default local base is `/`. `SITE_URL` defaults to `https://lenilsonjr.github.io` for generated site metadata. Configure the deployment origin for another host. Do not use the docs build to expose `means serve`.

The CI workflow builds, checks, and browser-tests the website on relevant changes. It does not deploy automatically. Choose a host and domain before publishing.

## Real terminal media

The landing page uses screenshots and silent videos captured from the running TUI. All ledger data is synthetic. See [`demo/README.md`](demo/README.md) for the seed generator, PTY recording, renderer, and reproduction steps.
