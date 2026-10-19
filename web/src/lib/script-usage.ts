/**
 * How each script language reaches a library plugin.
 *
 * One entry per language, and adding a language is adding an entry: nothing
 * else in the dashboard names a language. The list is fixed rather than read
 * from the installed interpreters, so a reader sees how the library is used
 * from whichever language they write, not only from the ones this instance
 * happens to run.
 */
export type ScriptLanguage = {
  /** The interpreter's `language_id`, which is also what `scripts.script_type` holds. */
  id: string
  label: string
  /** A Shiki bundled grammar id. */
  grammar: string
  /** The line that binds `namespace` to a local name. */
  importLine: (binding: string, namespace: string) => string
}

export const SCRIPT_LANGUAGES: ScriptLanguage[] = [
  {
    id: "lua",
    label: "Lua",
    grammar: "lua",
    importLine: (binding, namespace) =>
      `local ${binding} = require("${namespace}")`,
  },
  // JavaScript and TypeScript produce the same line: TypeScript adds types to
  // a script, not to how it imports. Both are listed anyway, so neither
  // reader has to assume the other's tab applies to them.
  {
    id: "javascript",
    label: "JavaScript",
    grammar: "javascript",
    importLine: (binding, namespace) =>
      `import ${binding} from "${namespace}";`,
  },
  {
    id: "typescript",
    label: "TypeScript",
    grammar: "typescript",
    importLine: (binding, namespace) =>
      `import ${binding} from "${namespace}";`,
  },
]

/**
 * The local name a namespace is bound to: its last segment, made safe to use
 * as an identifier so the line can be pasted as-is.
 */
export function bindingFor(namespace: string): string {
  const last = namespace.split(".").pop() ?? namespace
  const safe = last.replace(/[^A-Za-z0-9_$]/g, "_")
  return /^[0-9]/.test(safe) ? `_${safe}` : safe || "lib"
}
