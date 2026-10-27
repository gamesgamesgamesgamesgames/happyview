---
title: "Editor types"
---

`happyview types <root>` writes declaration files for the project at `<root>`, so your editor's own language server can complete and check what a script sees: each `happyview.*` library, a handler's input and return value from its lexicon, `ctx`, and records by NSID.

```bash
happyview types .                          # into .happyview/types/
happyview types . --out ./types            # somewhere else
happyview types . --plugin ./plugins/lua   # load this interpreter instead of the project's
```

It builds the project the way `happyview check` does, with no database and no network, then asks each interpreter the project uses to render the types in its own language. Each language gets its own directory, such as `.happyview/types/lua/` and `.happyview/types/typescript/`, holding the files your language server already reads: LuaLS `---@meta` files for Lua, `.d.ts` files for JavaScript and TypeScript.

Interpreters come from `--plugin`, else `$PLUGIN_DIR`, else `<root>/.happyview/plugins`, as for `check`. Libraries are loaded from the same places.

`happyview dev` keeps `.happyview/types/` current on its own: it writes the types once it starts watching, and again after every build that changes what a script can see, printing a `types:` line only when it wrote something or failed. A failed build leaves the last good types in place.

## What it prints

One line per interpreter:

| Line | Meaning |
| --- | --- |
| `types: lua: wrote 14 files` | The directory was replaced with a fresh set |
| `types: lua: up to date` | Nothing changed since the last run, so nothing was written |
| `types: lua: skipped: … needs happyview-plugin-sdk 0.4.0 or later for types` | The interpreter cannot render types |
| `types: lua: failed: …` | The interpreter failed; its directory is left as it was |

A library whose description does not parse is reported as `types: library <id> describes itself wrongly: …` and left out, and the other libraries are still typed.

A directory is replaced whole, so an editor never reads half of a set, and files from an earlier run that the interpreter no longer writes disappear. `manifest.json` in each directory records the interpreter and library versions and a hash of everything the types were made from; the next run compares it and writes nothing when they match, which keeps rerunning cheap.

## Exit codes

| Code | When |
| --- | --- |
| `0` | Every interpreter wrote, was up to date, or was skipped |
| `1` | The project does not build (nothing is written), or an interpreter failed |
| `2` | A usage error, or a plugin that will not load |

## Typing a handler

A language server cannot give a function's parameters types from outside its file, so a handler names its types once. The names come from the lexicon's NSID and the kind of script; each interpreter plugin documents its own. For a query `com.example.getThing`:

```lua
---@param input lexicons.com.example.getThing.Params
---@param ctx happyview.QueryCtx
---@return lexicons.com.example.getThing.Output
function handle(input, ctx)
end
```

```ts
import type { Params, Output } from "happyview:lexicons/com.example.getThing";
import type { QueryCtx } from "happyview:ctx";

export default async function handle(input: Params, ctx: QueryCtx): Promise<Output> {
}
```

Pointing your editor at `.happyview/types/` is the language server's own setting: `workspace.library` in `.luarc.json` for LuaLS, and `include` or `typeRoots` in `tsconfig.json`.
