import { defineConfig } from 'vitepress'
import { readdirSync, readFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parse as parseToml } from 'smol-toml'

const __dirname = dirname(fileURLToPath(import.meta.url))

interface SidebarItem { text: string; link: string }

function buildUpCloudSidebar() {
  const schemaDir = join(__dirname, '../../../src/providers/upcloud/schemas')
  let files: string[] = []
  try {
    files = readdirSync(schemaDir).filter((f) => f.endsWith('.toml'))
  } catch {
    files = []
  }

  // Group entries by their declared `section`. Within a section, list
  // resources before data sources, alphabetical within each kind.
  const sections = new Map<string, { resources: SidebarItem[]; dataSources: SidebarItem[] }>()

  for (const f of files) {
    const raw = readFileSync(join(schemaDir, f), 'utf8')
    const schema = parseToml(raw) as { section?: string }
    if (!schema.section) {
      throw new Error(
        `Schema '${f}' is missing required 'section' field. ` +
          `Every schema must declare which sidebar section it belongs to.`
      )
    }

    const slug = f.replace(/\.toml$/, '').replace(/^upcloud_/, '').replace(/_/g, '-')
    const display = slug.replace(/-data-source$/, '').replace(/-resource$/, '')
    const link = `/providers/upcloud/${slug}`

    let kind: 'resource' | 'data source' | null = null
    if (slug.endsWith('-data-source')) kind = 'data source'
    else if (slug.endsWith('-resource')) kind = 'resource'
    if (!kind) continue

    const item = { text: `${display} (${kind})`, link }
    const bucket = sections.get(schema.section) ?? { resources: [], dataSources: [] }
    if (kind === 'resource') bucket.resources.push(item)
    else bucket.dataSources.push(item)
    sections.set(schema.section, bucket)
  }

  const sortByText = (a: SidebarItem, b: SidebarItem) => a.text.localeCompare(b.text)

  const sidebar: { text: string; items: SidebarItem[] }[] = [
    {
      text: 'UpCloud (Provider)',
      items: [{ text: 'Overview', link: '/providers/upcloud/' }],
    },
  ]

  for (const name of Array.from(sections.keys()).sort()) {
    const { resources, dataSources } = sections.get(name)!
    sidebar.push({
      text: name,
      items: [...resources.sort(sortByText), ...dataSources.sort(sortByText)],
    })
  }

  return sidebar
}

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
    outline: [2, 3],
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
            { text: 'UpCloud', link: '/providers/upcloud/' },
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
      '/providers/upcloud/': buildUpCloudSidebar(),
    },

    socialLinks: [
      { icon: 'github', link: 'https://github.com/zerj9/blue' }
    ]
  }
})
