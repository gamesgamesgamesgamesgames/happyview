import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import type {
  CapabilityEntry,
  CapabilityReport,
  CapabilityRisk,
} from "@/types/plugins";

// A plugin is granted exactly what it declared, and that is what the
// consent dialog and the installed-plugins table show. One helper keeps
// that rule in one place.
export function visibleCapabilities(
  report: CapabilityReport,
): CapabilityEntry[] {
  return report.declared;
}

const RISK_LABEL: Record<CapabilityRisk, string> = {
  low: "Low risk",
  medium: "Medium risk",
  high: "High risk",
  critical: "Critical",
};

const RISK_VARIANT: Record<
  CapabilityRisk,
  "secondary" | "outline" | "default" | "destructive"
> = {
  low: "secondary",
  medium: "outline",
  high: "default",
  critical: "destructive",
};

export function PluginCapabilities({
  entries,
  allowedHosts,
}: {
  entries: CapabilityEntry[];
  allowedHosts?: string[];
}) {
  if (entries.length === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        This plugin asks for no permissions beyond logging.
      </p>
    );
  }
  const sorted = [...entries].sort((a, b) => rank(b.risk) - rank(a.risk));
  return (
    <ul className="space-y-2">
      {sorted.map((entry) => (
        <li
          key={entry.name}
          className="flex flex-col gap-1 rounded-md border p-2"
        >
          <div className="flex items-center gap-2">
            <code className="text-xs">{entry.name}</code>
            <Badge variant={RISK_VARIANT[entry.risk]}>
              {RISK_LABEL[entry.risk]}
            </Badge>
          </div>
          <p className="text-sm text-muted-foreground">{entry.description}</p>
          {entry.name === "network:request" &&
            allowedHosts &&
            allowedHosts.length > 0 && (
              <p className="text-xs text-muted-foreground">
                Hosts: {allowedHosts.join(", ")}
              </p>
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

/** What `next` declares that `installed` doesn't. */
export function newCapabilities(
  installed: CapabilityReport,
  next: CapabilityReport,
): CapabilityEntry[] {
  const held = new Set(visibleCapabilities(installed).map((c) => c.name));
  return visibleCapabilities(next).filter((c) => !held.has(c.name));
}

/**
 * Asks for consent to what a new version adds. The reload names everything
 * the new version declares, so the server grants no more than was shown
 * here; with nothing new there is nothing to ask.
 */
export function CapabilityConsent({
  id,
  entries,
  allowedHosts,
  accepted,
  onAcceptedChange,
  disabled,
}: {
  id: string;
  entries: CapabilityEntry[];
  allowedHosts?: string[];
  accepted: boolean;
  onAcceptedChange: (accepted: boolean) => void;
  disabled?: boolean;
}) {
  if (entries.length === 0) return null;
  return (
    <div className="flex flex-col gap-3 rounded-lg border p-4">
      <p className="text-sm font-medium">
        This version asks for new permissions
      </p>
      <PluginCapabilities entries={entries} allowedHosts={allowedHosts} />
      <label htmlFor={id} className="flex items-center gap-2 text-sm">
        <Checkbox
          id={id}
          checked={accepted}
          onCheckedChange={(v) => onAcceptedChange(v === true)}
          disabled={disabled}
        />
        Allow these permissions
      </label>
    </div>
  );
}

function rank(risk: CapabilityRisk): number {
  return { low: 0, medium: 1, high: 2, critical: 3 }[risk];
}
