import { readFile, writeFile } from 'node:fs/promises';
const notices = [
  ['Astro', 'astro/LICENSE'],
  ['Starlight', '@astrojs/starlight/LICENSE'],
  ['Expressive Code', '@expressive-code/core/LICENSE'],
  ['Pagefind', 'pagefind/LICENSE/LICENSE'],
  ['Inter', '@fontsource-variable/inter/LICENSE'],
  ['JetBrains Mono', '@fontsource-variable/jetbrains-mono/LICENSE'],
];
const sections = await Promise.all(
  notices.map(
    async ([name, file]) =>
      `${name}\n${'='.repeat(name.length)}\n\n${await readFile(new URL(`../node_modules/${file}`, import.meta.url), 'utf8')}`,
  ),
);
await writeFile(
  new URL('../public/third-party-notices.txt', import.meta.url),
  `means documentation website — third-party notices\n\nThese notices cover website components and bundled fonts. They do not describe the Rust application distribution.\n\n${sections.join('\n\n')}`,
);
