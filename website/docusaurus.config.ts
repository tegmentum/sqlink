import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

// This runs in Node.js  Don't use client-side code here (browser APIs, JSX...)

const config: Config = {
  title: 'sqlink',
  tagline: 'SQLite + WebAssembly extension runtime',
  favicon: 'img/favicon.ico',

  future: {
    v4: true,
  },

  url: 'https://tegmentum.github.io',
  baseUrl: '/sqlink/',

  organizationName: 'tegmentum',
  projectName: 'sqlink',

  onBrokenLinks: 'warn',

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  // PLAN docs use raw `<` for URLs + generics; parsing them as MDX
  // fails (looks like a JSX tag). 'detect' parses .md as commonmark
  // and .mdx as MDX  the concept pages keep MDX features; migrated
  // PLAN files (all .md) stay strict markdown.
  markdown: {
    format: 'detect',
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  themes: [
    [
      require.resolve('@easyops-cn/docusaurus-search-local'),
      {
        hashed: true,
        // Docs are served at the site root (routeBasePath: '/').
        docsRouteBasePath: '/',
        indexBlog: false,
        highlightSearchTermsOnTargetPage: true,
      },
    ],
  ],

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
          routeBasePath: '/',
          editUrl: 'https://github.com/tegmentum/sqlink/edit/main/website/',
        },
        // Blog disabled  v1 docs site, no blog posts yet.
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    colorMode: {
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'sqlink',
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          href: 'https://github.com/tegmentum/sqlink',
          label: 'GitHub',
          position: 'right',
        },
      ],
      hideOnScroll: false,
    },
    footer: {
      style: 'light',
      links: [
        {
          title: 'Source',
          items: [
            {
              label: 'GitHub',
              href: 'https://github.com/tegmentum/sqlink',
            },
          ],
        },
      ],
      copyright: `Copyright © ${new Date().getFullYear()} Tegmentum  Built with Docusaurus.`,
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
      additionalLanguages: ['toml', 'rust', 'bash', 'sql'],
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
