#!/usr/bin/env node
//
// Generate provider documentation pages from schema TOMLs.
//
// For each schema TOML at <provider>/schemas/<file>.toml:
//   - The sibling <file>.md is REQUIRED. It is the page content.
//   - The schema's `inputs` and `outputs` are rendered as markdown tables.
//   - The page's `<!-- @auto:inputs -->` and `<!-- @auto:outputs -->` markers
//     are replaced with those tables. If a marker is absent, the matching
//     table is appended at the end of the page (lax handling).
//
// Output: docs/docs/providers/<provider>/<slug>.md
// Generated files are gitignored; this script must run before vitepress.

import { readdirSync, readFileSync, writeFileSync, existsSync, mkdirSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'
import { parse as parseToml } from 'smol-toml'

const __dirname = dirname(fileURLToPath(import.meta.url))
const REPO_ROOT = join(__dirname, '../../')

const PROVIDERS = [
  {
    name: 'upcloud',
    schemaDir: join(REPO_ROOT, 'src/providers/upcloud/schemas'),
    outDir: join(REPO_ROOT, 'docs/docs/providers/upcloud'),
  },
]

function deriveSlug(filename, providerName) {
  return filename
    .replace(/\.toml$/, '')
    .replace(new RegExp(`^${providerName}_`), '')
    .replace(/_/g, '-')
}

function renderInputsTable(inputs) {
  if (!inputs || Object.keys(inputs).length === 0) {
    return '_(no inputs)_'
  }
  const rows = ['| Field | Type | Required | Force New | Description |', '|---|---|---|---|---|']
  for (const [name, def] of Object.entries(inputs)) {
    const required = def.required ? 'yes' : '—'
    const forceNew = def.force_new ? 'yes' : '—'
    const desc = (def.description ?? '').replace(/\|/g, '\\|')
    rows.push(`| \`${name}\` | ${def.type} | ${required} | ${forceNew} | ${desc} |`)
  }
  return rows.join('\n')
}

function renderOutputsTable(outputs) {
  if (!outputs || Object.keys(outputs).length === 0) {
    return '_(no outputs)_'
  }
  const rows = ['| Field | Type | Description |', '|---|---|---|']
  for (const [name, def] of Object.entries(outputs)) {
    const desc = (def.description ?? '').replace(/\|/g, '\\|')
    rows.push(`| \`${name}\` | ${def.type} | ${desc} |`)
  }
  return rows.join('\n')
}

function generatePage(schemaPath, mdPath) {
  if (!existsSync(mdPath)) {
    throw new Error(
      `Schema '${schemaPath}' has no sibling docs file '${mdPath}'. ` +
        `Every schema must ship documentation.`
    )
  }

  const schema = parseToml(readFileSync(schemaPath, 'utf8'))
  let content = readFileSync(mdPath, 'utf8')

  const inputsTable = renderInputsTable(schema.inputs)
  const outputsTable = renderOutputsTable(schema.outputs)

  const inputsMarker = '<!-- @auto:inputs -->'
  const outputsMarker = '<!-- @auto:outputs -->'
  const hadInputs = content.includes(inputsMarker)
  const hadOutputs = content.includes(outputsMarker)

  if (hadInputs) content = content.replace(inputsMarker, inputsTable)
  if (hadOutputs) content = content.replace(outputsMarker, outputsTable)

  const appendix = []
  if (!hadInputs) appendix.push(`## Inputs\n\n${inputsTable}`)
  if (!hadOutputs) appendix.push(`## Outputs\n\n${outputsTable}`)
  if (appendix.length) {
    content = `${content.trimEnd()}\n\n${appendix.join('\n\n')}\n`
  }

  return content
}

let generated = 0
for (const provider of PROVIDERS) {
  if (!existsSync(provider.schemaDir)) continue
  mkdirSync(provider.outDir, { recursive: true })

  const files = readdirSync(provider.schemaDir).filter((f) => f.endsWith('.toml'))
  for (const f of files) {
    const slug = deriveSlug(f, provider.name)
    const schemaPath = join(provider.schemaDir, f)
    const mdPath = join(provider.schemaDir, f.replace(/\.toml$/, '.md'))
    const outPath = join(provider.outDir, `${slug}.md`)

    const content = generatePage(schemaPath, mdPath)
    writeFileSync(outPath, content)
    generated += 1
    console.log(`gen-docs: ${provider.name}/${slug}.md`)
  }
}

console.log(`gen-docs: ${generated} page(s) generated`)
