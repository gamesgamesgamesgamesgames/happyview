"use client";

import { useCallback, useEffect, useState } from "react";
import { IconAlertTriangle } from "@tabler/icons-react";

import { useCurrentUser } from "@/hooks/use-current-user";
import { getFeatureFlags, setFeatureFlag, type FeatureFlag } from "@/lib/api";
import { SiteHeader } from "@/components/site-header";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";

export default function ExperimentsPage() {
  const { hasPermission } = useCurrentUser();
  const canManage = hasPermission("settings:manage");

  const [flags, setFlags] = useState<FeatureFlag[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [toggling, setToggling] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const data = await getFeatureFlags();
      setFlags(data);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleToggle(flag: FeatureFlag) {
    setError(null);
    setToggling(flag.key);
    try {
      await setFeatureFlag(flag.key, !flag.enabled);
      await load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setToggling(null);
    }
  }

  return (
    <>
      <SiteHeader title="Experiments" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6 max-w-3xl">
        <div className="flex items-start gap-3 rounded-lg border border-yellow-500/30 bg-yellow-500/5 p-4">
          <IconAlertTriangle className="mt-0.5 size-5 shrink-0 text-yellow-600 dark:text-yellow-500" />
          <div className="text-sm">
            <p className="font-medium text-yellow-600 dark:text-yellow-500">
              Experimental features
            </p>
            <p className="mt-1 text-muted-foreground">
              These features are under active development. They may be
              incomplete, change without notice, or be removed entirely.
              Disabling a feature hides its routes and UI — no data is deleted.
            </p>
          </div>
        </div>

        {error && <p className="text-destructive text-sm">{error}</p>}

        {flags.map((flag) => (
          <div
            key={flag.key}
            className="flex items-start justify-between gap-4 rounded-lg border p-4"
          >
            <div className="flex flex-col gap-1">
              <Label htmlFor={flag.key} className="text-sm font-medium">
                {flag.name}
              </Label>
              <p className="text-muted-foreground text-xs">
                {flag.description}
              </p>
            </div>
            <Switch
              id={flag.key}
              checked={flag.enabled}
              onCheckedChange={() => handleToggle(flag)}
              disabled={!canManage || toggling === flag.key}
            />
          </div>
        ))}

        {flags.length === 0 && !error && (
          <p className="text-muted-foreground text-sm">
            No experimental features available.
          </p>
        )}
      </div>
    </>
  );
}
