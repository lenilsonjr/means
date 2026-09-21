import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: process.env.SITE_URL || 'https://lenilsonjr.github.io',
  base: process.env.SITE_BASE || '/',
  trailingSlash: 'always',
  integrations: [
    starlight({
      title: 'means',
      description:
        'Local double-entry accounting. Your books, across entities and currencies, in your terminal.',
      logo: { src: './src/assets/mark.svg' },
      favicon: '/favicon.svg',
      social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/lenilsonjr/means' }],
      editLink: { baseUrl: 'https://github.com/lenilsonjr/means/edit/main/website/' },
      customCss: ['./src/styles/docs.css'],
      sidebar: [
        {
          label: 'Start here',
          items: [
            { label: 'Introduction', slug: 'docs/introduction' },
            { label: 'Installation & first run', slug: 'docs/getting-started' },
            { label: 'Core concepts', slug: 'docs/concepts' },
            { label: 'Terminal workflows', slug: 'docs/terminal' },
          ],
        },
        {
          label: 'Bring your data',
          items: [
            { slug: 'docs/guides/connections' },
            { slug: 'docs/guides/import-formats' },
            { slug: 'docs/guides/inter-wise' },
            { slug: 'docs/guides/mercury-treasury' },
          ],
        },
        {
          label: 'Keep your books',
          items: [
            { slug: 'docs/guides/expense-reports' },
            { slug: 'docs/guides/payee-aliases' },
            { slug: 'docs/guides/currency-migration' },
            { slug: 'docs/guides/export' },
            { slug: 'docs/guides/vault-sharing-usage' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { slug: 'docs/cli' },
            { slug: 'docs/guides/data-model' },
            { slug: 'docs/guides/vault-sharing-wire-v1' },
          ],
        },
        { label: 'Project', items: [{ slug: 'docs/contributing' }] },
      ],
    }),
  ],
});
