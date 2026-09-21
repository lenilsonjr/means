import { readdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';
const root = path.resolve('dist');
const base = `/${(process.env.SITE_BASE || '').replace(/^\/+|\/+$/g, '')}`.replace(/\/$/, '');
async function walk(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  return (
    await Promise.all(
      entries.map((e) => (e.isDirectory() ? walk(path.join(dir, e.name)) : path.join(dir, e.name))),
    )
  ).flat();
}
const files = await walk(root);
const htmlFiles = files.filter((file) => file.endsWith('.html'));
const errors = [];
for (const file of htmlFiles) {
  const html = await readFile(file, 'utf8');
  const relative = path.relative(root, file).replaceAll(path.sep, '/');
  const pageUrl = new URL(
    `${base}/${relative.replace(/index\.html$/, '')}`,
    'https://docs.invalid',
  );
  for (const [, value] of html.matchAll(/(?:href|src)="([^"]+)"/g)) {
    const url = new URL(value.replaceAll('&amp;', '&'), pageUrl);
    if (url.origin !== 'https://docs.invalid') continue;
    if (base && !url.pathname.startsWith(`${base}/`) && url.pathname !== base) {
      errors.push(`${relative}: escapes base: ${value}`);
      continue;
    }
    let target = path.join(root, decodeURIComponent(url.pathname.slice(base.length)));
    try {
      if ((await stat(target)).isDirectory()) target = path.join(target, 'index.html');
      const contents = await readFile(target, 'utf8');
      if (url.hash && target.endsWith('.html')) {
        const fragment = decodeURIComponent(url.hash.slice(1));
        if (!contents.includes(`id="${fragment}"`))
          errors.push(`${relative}: missing fragment ${value}`);
      }
    } catch {
      errors.push(`${relative}: missing target ${value}`);
    }
  }
}
if (errors.length) {
  console.error([...new Set(errors)].join('\n'));
  process.exit(1);
}
console.log(`Checked links on ${htmlFiles.length} static pages.`);
