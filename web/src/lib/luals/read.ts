// Reads the LuaLS declaration files an interpreter renders (`happyview
// types`, and `GET /admin/editor/declarations`). No LuaLS runs in a browser,
// so the dashboard reads the subset the renderer writes and nothing more:
// `---@meta`, `--- description`, `---@class`, `---@field`, `---@alias`,
// `---@param`, `---@return`, `local X = {}`, `function X.f(...)` and
// `function X:f(...)`, and `return X`.

export type LuaType =
  | { kind: "name"; name: string }
  | { kind: "literal"; value: string }
  | { kind: "array"; of: LuaType }
  | { kind: "union"; of: LuaType[] }
  | { kind: "map"; key: LuaType; value: LuaType }
  | { kind: "object"; fields: LuaField[] };

export interface LuaField {
  name: string;
  type: LuaType;
  optional: boolean;
  description?: string;
}

export interface LuaParam {
  name: string;
  type: LuaType;
  optional: boolean;
  description?: string;
}

export interface LuaFunction {
  name: string;
  /** Declared with `:`, so it is called with `:`. */
  method: boolean;
  params: LuaParam[];
  returns?: LuaType;
  description?: string;
}

export interface LuaClass {
  name: string;
  description?: string;
  fields: LuaField[];
  functions: LuaFunction[];
}

export interface LuaDeclarations {
  classes: Map<string, LuaClass>;
  aliases: Map<string, LuaType>;
  /** What `require(name)` returns: a module name to its class. */
  modules: Map<string, string>;
}

export interface DeclarationFile {
  path: string;
  contents: string;
}

export function emptyDeclarations(): LuaDeclarations {
  return { classes: new Map(), aliases: new Map(), modules: new Map() };
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/** Parse the type at the start of `text`. Answers the type and the index
 * where it ends, so whatever follows is the description. */
export function parseType(
  text: string,
  from = 0,
): [LuaType, number] | undefined {
  let i = from;
  const skip = () => {
    while (i < text.length && text[i] === " ") i++;
  };

  const primary = (): LuaType | undefined => {
    skip();
    const ch = text[i];
    if (ch === '"' || ch === "'") {
      const end = text.indexOf(ch, i + 1);
      if (end < 0) return undefined;
      const value = text.slice(i + 1, end);
      i = end + 1;
      return { kind: "literal", value };
    }
    if (ch === "(") {
      i++;
      const inner = union();
      skip();
      if (text[i] !== ")") return undefined;
      i++;
      return inner;
    }
    if (ch === "{") {
      i++;
      const fields: LuaField[] = [];
      for (;;) {
        skip();
        if (text[i] === "}") {
          i++;
          return { kind: "object", fields };
        }
        const m = /^(\[?"?([$\w]+)"?\]?)(\??)\s*:/.exec(text.slice(i));
        if (!m) return undefined;
        i += m[0].length;
        const type = union();
        if (!type) return undefined;
        fields.push({ name: m[2], type, optional: m[3] === "?" });
        skip();
        if (text[i] === ",") i++;
      }
    }
    const m = /^[A-Za-z_][\w.]*/.exec(text.slice(i));
    if (!m) return undefined;
    i += m[0].length;
    if (m[0] === "table" && text[i] === "<") {
      i++;
      const key = union();
      skip();
      if (!key || text[i] !== ",") return undefined;
      i++;
      const value = union();
      skip();
      if (!value || text[i] !== ">") return undefined;
      i++;
      return { kind: "map", key, value };
    }
    return { kind: "name", name: m[0] };
  };

  const postfix = (): LuaType | undefined => {
    let type = primary();
    if (!type) return undefined;
    for (;;) {
      if (text.startsWith("[]", i)) {
        i += 2;
        type = { kind: "array", of: type };
      } else if (text[i] === "?") {
        i++;
        type = { kind: "union", of: [type, { kind: "name", name: "nil" }] };
      } else {
        return type;
      }
    }
  };

  const union = (): LuaType | undefined => {
    const first = postfix();
    if (!first) return undefined;
    const of = [first];
    for (;;) {
      const save = i;
      skip();
      if (text[i] !== "|") {
        i = save;
        break;
      }
      i++;
      const next = postfix();
      if (!next) return undefined;
      of.push(next);
    }
    return of.length === 1
      ? of[0]
      : {
          kind: "union",
          of: of.flatMap((t) => (t.kind === "union" ? t.of : [t])),
        };
  };

  const type = union();
  return type ? [type, i] : undefined;
}

export function printType(type: LuaType): string {
  switch (type.kind) {
    case "name":
      return type.name;
    case "literal":
      return JSON.stringify(type.value);
    case "array": {
      const inner = printType(type.of);
      return type.of.kind === "union" ? `(${inner})[]` : `${inner}[]`;
    }
    case "union":
      return type.of.map(printType).join("|");
    case "map":
      return `table<${printType(type.key)}, ${printType(type.value)}>`;
    case "object":
      return `{ ${type.fields.map((f) => `${f.name}${f.optional ? "?" : ""}: ${printType(f.type)}`).join(", ")} }`;
  }
}

/** `name?` and the type after it, and the description after that. */
function nameTypeDescription(
  rest: string,
):
  | { name: string; optional: boolean; type: LuaType; description?: string }
  | undefined {
  const m = /^(\["([^"]+)"\]|[\w$]+)(\??)\s+/.exec(rest);
  if (!m) return undefined;
  const parsed = parseType(rest, m[0].length);
  if (!parsed) return undefined;
  const description = rest.slice(parsed[1]).trim() || undefined;
  return {
    name: m[2] ?? m[1],
    optional: m[3] === "?",
    type: parsed[0],
    description,
  };
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

export function readDeclarations(files: DeclarationFile[]): LuaDeclarations {
  const out = emptyDeclarations();
  for (const file of files) {
    if (file.path.endsWith(".lua")) readFile(file.contents, out);
  }
  return out;
}

function readFile(contents: string, out: LuaDeclarations): void {
  let meta: string | undefined;
  let description: string[] = [];
  let params: LuaParam[] = [];
  let returns: LuaType | undefined;
  let current: LuaClass | undefined;
  /** Which class a `local` names, for the functions defined on it. */
  const locals = new Map<string, LuaClass>();

  const reset = () => {
    description = [];
    params = [];
    returns = undefined;
  };

  for (const raw of contents.split("\n")) {
    const line = raw.trimEnd();
    let m: RegExpExecArray | null;
    if ((m = /^---@meta(?:\s+(\S+))?$/.exec(line))) {
      meta = m[1];
    } else if ((m = /^---@class\s+([\w.]+)/.exec(line))) {
      current = {
        name: m[1],
        description: description.join("\n") || undefined,
        fields: [],
        functions: [],
      };
      out.classes.set(current.name, current);
      reset();
    } else if ((m = /^---@field\s+(.*)$/.exec(line))) {
      const field = nameTypeDescription(m[1]);
      if (field && current) current.fields.push(field);
    } else if ((m = /^---@alias\s+([\w.]+)\s+(.*)$/.exec(line))) {
      const parsed = parseType(m[2]);
      if (parsed) out.aliases.set(m[1], parsed[0]);
      reset();
    } else if ((m = /^---@param\s+(.*)$/.exec(line))) {
      const param = nameTypeDescription(m[1]);
      if (param) params.push(param);
    } else if ((m = /^---@return\s+(.*)$/.exec(line))) {
      returns = parseType(m[1])?.[0];
    } else if ((m = /^--- ?(.*)$/.exec(line)) && !line.startsWith("---@")) {
      description.push(m[1]);
    } else if ((m = /^local\s+(\w+)\s*=\s*\{\s*\}$/.exec(line))) {
      if (current) locals.set(m[1], current);
      current = undefined;
      reset();
    } else if ((m = /^function\s+(\w+)([.:])(\w+)\s*\(/.exec(line))) {
      const owner = locals.get(m[1]);
      owner?.functions.push({
        name: m[3],
        method: m[2] === ":",
        params,
        returns,
        description: description.join("\n") || undefined,
      });
      reset();
    } else if ((m = /^return\s+(\w+)$/.exec(line))) {
      const owner = locals.get(m[1]);
      if (meta && owner) out.modules.set(meta, owner.name);
    } else if (line === "") {
      // A blank line ends a description that no declaration took.
      if (!current) reset();
    }
  }
  // A module file that returns nothing still names its class.
  if (meta && !out.modules.has(meta) && out.classes.has(meta))
    out.modules.set(meta, meta);
}
