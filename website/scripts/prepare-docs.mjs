import { readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { guides } from './guides.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const output = path.join(root, 'website/src/content/docs/docs/guides');
const base = `/${(process.env.SITE_BASE || '').replace(/^\/+|\/+$/g, '')}`.replace(/\/$/, '');
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
for (const guide of guides) {
  let body = await readFile(path.join(root, 'docs', `${guide.file}.md`), 'utf8');
  body = body.replace(/^# .+\r?\n/, '');
  // Published guides link to other published guides. Keep source-code links on GitHub.
  body = body.replace(/\]\(([^\s)]+)([^)]*)\)/g, (whole, href, title) => {
    if (/^(?:[a-z]+:|#|\/)/i.test(href)) return whole;
    const [file, fragment] = href.split('#');
    const source = path.posix.normalize(path.posix.join('docs', file));
    const published = guides.find((candidate) => source === `docs/${candidate.file}.md`);
    if (source.startsWith('docs/') && !published) {
      throw new Error(`${guide.file}: link to unpublished documentation: ${href}`);
    }
    const target = published
      ? `${base}/docs/guides/${published.file}/`
      : `https://github.com/lenilsonjr/means/blob/main/${source}`;
    return `](${target}${fragment ? `#${fragment}` : ''}${title})`;
  });
  const frontmatter = `---\ntitle: ${JSON.stringify(guide.title)}\ndescription: ${JSON.stringify(guide.description)}\neditUrl: https://github.com/lenilsonjr/means/edit/main/docs/${guide.file}.md\n---\n`;
  await writeFile(path.join(output, `${guide.file}.md`), frontmatter + body);
}
console.log(`Prepared ${guides.length} guides from docs/.`);
await import('./prepare-notices.mjs');
