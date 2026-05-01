import { defineConfig } from 'vitepress'

const guideSidebar = [
  {
    text: 'Configuration',
    items: [
      { text: 'Overview', link: '/config/' },
      { text: 'Parameters', link: '/config/parameters' },
      { text: 'Data Sources', link: '/config/data-sources' },
      { text: 'Resources', link: '/config/resources' },
      { text: 'Templates', link: '/config/templates' },
      { text: 'Encryption', link: '/config/encryption' },
      { text: 'Provider config', link: '/config/providers' },
      { text: 'State', link: '/config/state' },
    ]
  },
  {
    text: 'CLI',
    items: [
      { text: 'Commands', link: '/cli/' },
    ]
  },
]

export default defineConfig({
  title: "Blue",
  description: "Infrastructure as Code in TOML",
  themeConfig: {
    nav: [
      { text: 'Guide', link: '/config/' },
      { text: 'Providers', link: '/providers/' },
    ],

    sidebar: {
      '/config/': guideSidebar,
      '/cli/': guideSidebar,
      '/providers/': [
        {
          text: 'Providers',
          items: [
            { text: 'Overview', link: '/providers/' },
            { text: 'Blue', link: '/providers/blue/' },
          ]
        }
      ],
      '/providers/blue/': [
        {
          text: 'Blue (Provider)',
          items: [
            { text: 'Overview', link: '/providers/blue/' },
          ]
        },
        {
          text: 'Resources',
          items: [
            { text: 'script', link: '/providers/blue/script-resource' },
          ]
        },
        {
          text: 'Data Sources',
          items: [
            { text: 'script', link: '/providers/blue/script-data-source' },
          ]
        },
      ],
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/zerj9/blue' }
    ]
  }
})
