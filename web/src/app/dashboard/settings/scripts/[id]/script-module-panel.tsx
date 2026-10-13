import type { ScriptModuleRow } from "@/types/scripts";

/**
 * What a compiled module's handler shows instead of an editor. There is no
 * upload and no save: the module is built from the project's source, and a
 * binary edited here would have no source anyone could review or rebuild.
 */
export function ScriptModulePanel({ module }: { module: ScriptModuleRow }) {
  const rows: [string, string][] = [
    ["Module", module.module],
    ["Path", module.path],
    ["Handler", module.handler],
    ["Build", module.hash.slice(0, 12)],
  ];
  return (
    <section
      aria-label="Compiled module"
      className="flex flex-col gap-3 rounded-lg border p-4"
    >
      <p className="text-sm text-muted-foreground">
        This trigger is handled by compiled code in the project. Change it in{" "}
        <span className="font-mono">{module.path}</span> and rebuild.
      </p>
      <dl className="grid grid-cols-[max-content_1fr] gap-x-6 gap-y-2 text-sm">
        {rows.map(([label, value]) => (
          <div key={label} className="contents">
            <dt className="text-muted-foreground">{label}</dt>
            <dd
              className="font-mono break-all"
              title={label === "Build" ? module.hash : undefined}
            >
              {value}
            </dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
