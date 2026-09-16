import { Badge } from "@/components/ui/badge";
import type { CapabilityEntry, CapabilityReport, CapabilityRisk } from "@/types/plugins";

// A plugin is granted exactly what it declared, and that is what the
// consent dialog and the installed-plugins table show. One helper keeps
// that rule in one place.
export function visibleCapabilities(report: CapabilityReport): CapabilityEntry[] {
  return report.declared;
}

const RISK_LABEL: Record<CapabilityRisk, string> = {
  low: "Low risk",
  medium: "Medium risk",
  high: "High risk",
  critical: "Critical",
};

const RISK_VARIANT: Record<CapabilityRisk, "secondary" | "outline" | "default" | "destructive"> = {
  low: "secondary",
  medium: "outline",
  high: "default",
  critical: "destructive",
};

export function PluginCapabilities({ entries, allowedHosts }: { entries: CapabilityEntry[]; allowedHosts?: string[] }) {
  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground">This plugin asks for no permissions beyond logging.</p>;
  }
  const sorted = [...entries].sort((a, b) => rank(b.risk) - rank(a.risk));
  return (
    <ul className="space-y-2">
      {sorted.map((entry) => (
        <li key={entry.name} className="flex flex-col gap-1 rounded-md border p-2">
          <div className="flex items-center gap-2">
            <code className="text-xs">{entry.name}</code>
            <Badge variant={RISK_VARIANT[entry.risk]}>{RISK_LABEL[entry.risk]}</Badge>
          </div>
          <p className="text-sm text-muted-foreground">{entry.description}</p>
          {entry.name === "network:request" && allowedHosts && allowedHosts.length > 0 && (
            <p className="text-xs text-muted-foreground">Hosts: {allowedHosts.join(", ")}</p>
          )}
          {entry.name === "network:request:defined" && (
            <p className="text-xs text-muted-foreground">
              {allowedHosts && allowedHosts.length > 0
                ? `Hosts: ${allowedHosts.join(", ")}`
                : "no hosts configured yet"}
            </p>
          )}
        </li>
      ))}
    </ul>
  );
}

function rank(risk: CapabilityRisk): number {
  return { low: 0, medium: 1, high: 2, critical: 3 }[risk];
}
