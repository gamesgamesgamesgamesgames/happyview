// What an expression in a script refers to, read off the declarations: the
// part of a language server the dashboard needs for completion, hover and
// signature help.

import {
  printType,
  type LuaClass,
  type LuaDeclarations,
  type LuaField,
  type LuaFunction,
  type LuaType,
} from "./read";

const named = (name: string): LuaType => ({ kind: "name", name });

/** The types `handle(input, ctx)` receives for a trigger id. */
export function handlerTypes(
  decls: LuaDeclarations,
  trigger: string | undefined,
): { input?: LuaType; ctx?: LuaType } {
  const [kind, nsid] = (trigger ?? "").split(":");
  const lexicon = (part: string, fallback?: string) => {
    const name = `lexicons.${nsid}.${part}`;
    if (decls.classes.has(name)) return named(name);
    return fallback ? named(fallback) : undefined;
  };
  switch (kind) {
    case "xrpc.query":
      return { input: lexicon("Params"), ctx: named("happyview.QueryCtx") };
    case "xrpc.procedure":
      return { input: lexicon("Input"), ctx: named("happyview.ProcedureCtx") };
    case "record.index":
    case "record.create":
    case "record.update":
    case "record.delete":
      return {
        input: lexicon("Event", "happyview.RecordEvent"),
        ctx: named("happyview.RecordCtx"),
      };
    case "labeler.apply":
      return {
        input: named("happyview.LabelEvent"),
        ctx: named("happyview.LabelCtx"),
      };
    case "job.run":
      return { ctx: named("happyview.JobCtx") };
    default:
      return {};
  }
}

export interface Segment {
  name: string;
  /** Followed by a call, so the step is the function's return type. */
  call: boolean;
  /** Reached with `:` rather than `.`. */
  colon: boolean;
}

/** The chain of names `text` ends in, read backward: `db.records("x"):where`
 * is `db`, `records()`, `:where`. Answers undefined when it ends in no name. */
export function trailingChain(text: string): Segment[] | undefined {
  const segments: Segment[] = [];
  let i = text.length;
  const skipSpace = () => {
    while (i > 0 && /\s/.test(text[i - 1])) i--;
  };
  for (;;) {
    let call = false;
    skipSpace();
    if (text[i - 1] === ")") {
      let depth = 0;
      for (i = i - 1; i >= 0; i--) {
        if (text[i] === ")") depth++;
        else if (text[i] === "(" && --depth === 0) break;
      }
      if (i < 0) return undefined;
      call = true;
      skipSpace();
    }
    const end = i;
    while (i > 0 && /\w/.test(text[i - 1])) i--;
    const name = text.slice(i, end);
    if (!name || /^\d/.test(name)) return undefined;
    skipSpace();
    const sep = text[i - 1];
    const colon = sep === ":" && text[i - 2] !== ":";
    segments.unshift({ name, call, colon });
    if (sep === "." && text[i - 2] !== ".") {
      i--;
      continue;
    }
    if (colon) {
      i--;
      continue;
    }
    return segments;
  }
}

export interface Members {
  fields: LuaField[];
  functions: LuaFunction[];
}

/** The fields and functions of a type: a class, an inline object, an alias
 * of one, or the first member of a union that has any. */
export function membersOf(
  decls: LuaDeclarations,
  type: LuaType | undefined,
  depth = 0,
): Members | undefined {
  if (!type || depth > 8) return undefined;
  switch (type.kind) {
    case "name": {
      const cls: LuaClass | undefined = decls.classes.get(type.name);
      if (cls) return { fields: cls.fields, functions: cls.functions };
      return membersOf(decls, decls.aliases.get(type.name), depth + 1);
    }
    case "object":
      return { fields: type.fields, functions: [] };
    case "union":
      for (const member of type.of) {
        const found = membersOf(decls, member, depth + 1);
        if (found) return found;
      }
      return undefined;
    default:
      return undefined;
  }
}

/** The element type of an array, through an alias or a nullable union. */
export function elementOf(
  decls: LuaDeclarations,
  type: LuaType | undefined,
  depth = 0,
): LuaType | undefined {
  if (!type || depth > 8) return undefined;
  if (type.kind === "array") return type.of;
  if (type.kind === "union") {
    for (const member of type.of) {
      const found = elementOf(decls, member, depth + 1);
      if (found) return found;
    }
  }
  if (type.kind === "name")
    return elementOf(decls, decls.aliases.get(type.name), depth + 1);
  return undefined;
}

export type Bindings = Map<string, LuaType>;

/** One step along a chain. */
function step(
  decls: LuaDeclarations,
  from: LuaType | undefined,
  segment: Segment,
): LuaType | undefined {
  const members = membersOf(decls, from);
  if (!members) return undefined;
  const fn = members.functions.find((f) => f.name === segment.name);
  if (fn) return segment.call ? fn.returns : undefined;
  const field = members.fields.find((f) => f.name === segment.name);
  return field && !segment.call ? field.type : undefined;
}

/** The type a whole chain names, from bindings for its root. */
export function resolveChain(
  decls: LuaDeclarations,
  bindings: Bindings,
  chain: Segment[],
): LuaType | undefined {
  const [root, ...rest] = chain;
  if (!root || root.call) return undefined;
  let type = bindings.get(root.name);
  for (const segment of rest) {
    type = step(decls, type, segment);
    if (!type) return undefined;
  }
  return type;
}

/** The member a chain's last name refers to, and the type it is on. */
export function resolveMember(
  decls: LuaDeclarations,
  bindings: Bindings,
  chain: Segment[],
): { field?: LuaField; fn?: LuaFunction } | undefined {
  if (chain.length < 2) return undefined;
  const owner = resolveChain(decls, bindings, chain.slice(0, -1));
  const members = membersOf(decls, owner);
  const last = chain[chain.length - 1];
  if (!members) return undefined;
  const fn = members.functions.find((f) => f.name === last.name);
  if (fn) return { fn };
  const field = members.fields.find((f) => f.name === last.name);
  return field ? { field } : undefined;
}

/** What each name in a script holds, as far as the declarations can say:
 * `input` and `ctx` from the trigger, `local x = require("…")`, `local x =
 * <chain>(…)`, and `for _, x in ipairs(<chain>)`. Read top to bottom, so a
 * binding may use any above it. */
export function scriptBindings(
  decls: LuaDeclarations,
  source: string,
  trigger: string | undefined,
): Bindings {
  const bindings: Bindings = new Map();
  const { input, ctx } = handlerTypes(decls, trigger);
  if (input) bindings.set("input", input);
  if (ctx) bindings.set("ctx", ctx);

  for (const line of source.split("\n")) {
    let m: RegExpExecArray | null;
    if (
      (m =
        /^\s*local\s+(\w+)\s*=\s*require\s*\(?\s*["']([\w.]+)["']\s*\)?/.exec(
          line,
        ))
    ) {
      const cls = decls.modules.get(m[2]);
      if (cls) bindings.set(m[1], named(cls));
    } else if ((m = /^\s*local\s+(\w+)\s*=\s*(.+?)\s*$/.exec(line))) {
      const chain = trailingChain(m[2]);
      const type = chain && resolveChain(decls, bindings, chain);
      if (type) bindings.set(m[1], type);
    } else if (
      (m = /^\s*for\s+\w+\s*,\s*(\w+)\s+in\s+i?pairs\s*\((.+)\)\s*do/.exec(
        line,
      ))
    ) {
      const chain = trailingChain(m[2]);
      const element = elementOf(
        decls,
        chain && resolveChain(decls, bindings, chain),
      );
      if (element) bindings.set(m[1], element);
    }
  }
  return bindings;
}

/** The record collections the declarations describe, for a `collection`
 * argument. */
export function recordCollections(decls: LuaDeclarations): string[] {
  return [...decls.classes.keys()]
    .filter((name) => name.startsWith("lexicons.") && name.endsWith(".Record"))
    .map((name) => name.slice("lexicons.".length, -".Record".length))
    .sort();
}

export function paramLabel(p: {
  name: string;
  optional: boolean;
  type: LuaType;
}): string {
  return `${p.name}${p.optional ? "?" : ""}: ${printType(p.type)}`;
}

/** `function db.records(collection: string): happyview.db.records.Object`. */
export function signature(fn: LuaFunction, owner?: string): string {
  const head = owner ? `${owner}${fn.method ? ":" : "."}${fn.name}` : fn.name;
  const returns = fn.returns ? `: ${printType(fn.returns)}` : "";
  return `function ${head}(${fn.params.map(paramLabel).join(", ")})${returns}`;
}
