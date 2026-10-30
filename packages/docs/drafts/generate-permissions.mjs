// Writes the permissions table into glossary.mdx, between its
// "BEGIN generated: permissions" and "END generated: permissions" markers,
// from src/admin/permissions.rs: each permission's key, name, description and
// category (`Permission::info`), and which built-in templates grant it
// (`Template::permissions`). Run from anywhere:
//
//   node packages/docs/drafts/generate-permissions.mjs [--check]
//
// --check exits non-zero instead of writing when the table is out of date.
import { readFileSync, writeFileSync } from 'node:fs'

const source = readFileSync(new URL('../../../src/admin/permissions.rs', import.meta.url), 'utf8')
const glossaryUrl = new URL('./glossary.mdx', import.meta.url)

// `Self::LexiconsCreate => PermissionInfo { key: "...", name: "...", description: "...", category: "..." }`
const info = new Map()
for (const m of source.matchAll(
	/Self::(\w+) => PermissionInfo \{\s*key: "([^"]+)",\s*name: "([^"]+)",\s*description: "([^"]+)",\s*category: "([^"]+)",?\s*\}/g,
)) {
	const [, variant, key, name, description, category] = m
	info.set(variant, { key, name, description, category })
}

const allBody = source.match(/pub fn all\(\) -> HashSet<Permission> \{([\s\S]*?)\n {4}\}/)
if (!allBody) throw new Error('Permission::all() not found')
const all = [...allBody[1].matchAll(/Self::(\w+)/g)].map((m) => m[1])
for (const v of all) if (!info.has(v)) throw new Error(`no PermissionInfo for ${v}`)
if (info.size !== all.length) throw new Error(`info() has ${info.size} entries, all() has ${all.length}`)

// The templates nest: Viewer is a list, Operator and Manager insert into the
// one before them, Full Access is all().
const templatesBody = source.slice(source.indexOf('pub fn permissions(&self) -> HashSet<Permission>'))
const section = (from, to) => templatesBody.slice(templatesBody.indexOf(from), templatesBody.indexOf(to))
const variants = (text) => [...text.matchAll(/Permission::(\w+)/g)].map((m) => m[1])
const viewer = new Set(variants(section('Self::Viewer =>', 'Self::Operator =>')))
const operator = new Set([...viewer, ...variants(section('Self::Operator =>', 'Self::Manager =>'))])
const manager = new Set([...operator, ...variants(section('Self::Manager =>', 'Self::FullAccess =>'))])
if (!/Self::FullAccess => Permission::all\(\)/.test(templatesBody)) throw new Error('Full Access is no longer all()')

// The permission set's level: the lowest template that grants it.
const lowest = (v) => (viewer.has(v) ? 'Viewer' : operator.has(v) ? 'Operator' : manager.has(v) ? 'Manager' : 'Full Access')

const byCategory = new Map()
for (const v of all) {
	const p = info.get(v)
	if (!byCategory.has(p.category)) byCategory.set(p.category, [])
	byCategory.get(p.category).push({ ...p, lowest: lowest(v) })
}

const lines = [
	`HappyView has **${all.length} permissions**. A user holds any mix of them; the four built-in templates are starting points. Each template includes everything in the one before it: **Viewer** (${viewer.size}), **Operator** (${operator.size}), **Manager** (${manager.size}) and **Full Access** (all ${all.length}). The super user holds every permission and can't be restricted.`,
	'',
	'| Permission | What it allows | Lowest template that grants it |',
	'|---|---|---|',
]
for (const [category, perms] of byCategory) {
	lines.push(`| **${category}** | | |`)
	for (const p of perms) lines.push(`| \`${p.key}\` | ${p.description} | ${p.lowest} |`)
}
const table = lines.join('\n')

const begin = '{/* BEGIN generated: permissions (generate-permissions.mjs) */}'
const end = '{/* END generated: permissions */}'
const glossary = readFileSync(glossaryUrl, 'utf8')
const start = glossary.indexOf(begin)
const stop = glossary.indexOf(end)
if (start < 0 || stop < start) throw new Error('glossary.mdx is missing the generated-permissions markers')
const next = `${glossary.slice(0, start + begin.length)}\n\n${table}\n\n${glossary.slice(stop)}`

if (process.argv.includes('--check')) {
	if (next !== glossary) {
		console.error('glossary.mdx permissions table is out of date; run generate-permissions.mjs')
		process.exit(1)
	}
} else {
	writeFileSync(glossaryUrl, next)
	console.log(`wrote ${all.length} permissions in ${byCategory.size} categories`)
}
