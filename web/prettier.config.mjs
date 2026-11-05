/**
 * Prettier's own defaults, with the one the dashboard was actually split on
 * recorded explicitly: shadcn's generator emits components without
 * semicolons, hand-written files mostly had them, and nothing ever chose.
 * Semicolons win on weight of code.
 *
 * The scope is `web/` alone. Nothing formats the SDK packages, whose
 * published output is a separate decision, and `.gitignore`, the lockfile and
 * build output are already excluded by prettier itself.
 */
export default {
  semi: true,
};
