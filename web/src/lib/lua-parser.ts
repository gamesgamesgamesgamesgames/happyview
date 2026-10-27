/** Extract Lua identifier names (variables, functions, parameters). */
export function parseLuaIdentifiers(source: string): Set<string> {
  const ids = new Set<string>();
  // local var, local var = ..., local var1, var2 = ...
  for (const m of source.matchAll(/\blocal\s+([\w,\s]+?)(?:\s*=|$)/gm)) {
    for (const name of m[1].split(",")) {
      const trimmed = name.trim();
      if (trimmed && /^\w+$/.test(trimmed)) ids.add(trimmed);
    }
  }
  // function name(...), local function name(...)
  for (const m of source.matchAll(/\bfunction\s+(\w+)\s*\(([^)]*)\)/g)) {
    ids.add(m[1]);
    for (const p of m[2].split(",")) {
      const trimmed = p.trim();
      if (trimmed && /^\w+$/.test(trimmed)) ids.add(trimmed);
    }
  }
  // for var [, var...] in/=
  for (const m of source.matchAll(/\bfor\s+([\w,\s]+?)\s+in\b/g)) {
    for (const name of m[1].split(",")) {
      const trimmed = name.trim();
      if (trimmed && /^\w+$/.test(trimmed)) ids.add(trimmed);
    }
  }
  return ids;
}
