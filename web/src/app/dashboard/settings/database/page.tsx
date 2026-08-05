"use client";

import { useCallback, useEffect, useState } from "react";
import { toast } from "sonner";

import {
  cancelVacuum,
  getDatabaseStatus,
  scheduleVacuum,
  type DatabaseStatus,
} from "@/lib/api";
import {
  formatBytes as bytes,
  formatBytesParts,
  toastError,
} from "@/lib/format";
import { useVacuumStatus } from "@/components/vacuum-prompt-provider";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

/** Shared styling for the value cells in the Storage card.
 *
 * `font-mono` is explicit rather than inherited: `<code>` gets it from the
 * preflight defaults but `<data>` does not, and the column alignment depends on
 * every cell being monospace. Stating it here keeps the two in step. */
const CELL = "bg-muted rounded px-1 py-0.5 text-[11px] font-mono";

/** Inline code styling for a literal path — something you might copy or paste
 * into a shell. Matches the pattern used on the XRPC proxy settings page. */
function Mono({ children }: { children: React.ReactNode }) {
  return <code className={CELL}>{children}</code>;
}

/**
 * A byte count in a fixed-width cell.
 *
 * `<data>` rather than `<code>`: a size is a value with a machine-readable
 * form, not source text. The `value` attribute carries the exact byte count and
 * `title` surfaces it on hover, so "001.0 GiB" is still answerable when you need
 * to know whether that is 1,073,741,824 bytes or something that merely rounds
 * to it — which matters when you are deciding if a rebuild will fit on disk.
 *
 * The leading zeros only reserve width, so they are de-emphasised and hidden
 * from assistive tech: at full contrast they read as significant digits, and
 * read aloud they turn "3.1 MiB" into "zero zero three point one".
 */
function AlignedBytes({ n }: { n: number | null | undefined }) {
  const { pad, rest } = formatBytesParts(n);
  const body = (
    <>
      {pad && (
        <span className="text-muted-foreground" aria-hidden="true">
          {pad}
        </span>
      )}
      {rest}
    </>
  );

  // null means the measurement failed and undefined means it has not loaded —
  // neither has an exact value to expose, so no <data> and no misleading title.
  if (typeof n !== "number") {
    return (
      <span
        className={CELL}
        title={n === null ? "Could not be measured" : undefined}
      >
        {body}
      </span>
    );
  }

  return (
    <data
      className={CELL}
      value={String(n)}
      title={`${n.toLocaleString()} bytes`}
    >
      {body}
    </data>
  );
}

export default function DatabaseSettingsPage() {
  const [status, setStatus] = useState<DatabaseStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The restart-banner reason is owned by VacuumPromptProvider (globally
  // mounted, so it reflects a vacuum armed in another session/tab even when
  // this page was never opened) — this page just tells it to re-fetch right
  // after a successful schedule/cancel so the banner updates immediately
  // instead of waiting for the provider's next poll.
  const { refresh: refreshVacuumStatus } = useVacuumStatus();

  const refresh = useCallback(async () => {
    try {
      const next = await getDatabaseStatus();
      setStatus(next);
      setError(null);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
      toastError("Failed to load database status", e);
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const onSchedule = async () => {
    setBusy(true);
    try {
      await scheduleVacuum();
      toast.success("Cleanup scheduled", {
        description: "It will run the next time this instance restarts.",
      });
      await Promise.all([refresh(), refreshVacuumStatus()]);
    } catch (e: unknown) {
      toastError("Failed to schedule cleanup", e);
    } finally {
      setBusy(false);
    }
  };

  const onCancel = async () => {
    setBusy(true);
    try {
      await cancelVacuum();
      toast.success("Cleanup cancelled");
      await Promise.all([refresh(), refreshVacuumStatus()]);
    } catch (e: unknown) {
      toastError("Failed to cancel cleanup", e);
    } finally {
      setBusy(false);
    }
  };

  if (!status) {
    return (
      <>
        <SiteHeader title="Database Settings" />
        <div className="flex flex-col gap-4 p-4 lg:p-6 max-w-3xl">
          {error ? (
            <div className="flex flex-col gap-2">
              <p className="text-destructive text-sm">{error}</p>
              <div>
                <Button variant="outline" onClick={refresh}>
                  Retry
                </Button>
              </div>
            </div>
          ) : (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
        </div>
      </>
    );
  }

  const sqlite = status.backend === "sqlite";
  const insufficient =
    status.feasibility?.status === "insufficient" ? status.feasibility : null;
  const unmeasurable =
    status.feasibility?.status === "unknown" ? status.feasibility : null;
  // Fail closed: only the explicit "ok" variant permits scheduling. Anything
  // else — insufficient, unknown, or a future variant this UI doesn't know
  // about yet — must not silently fall through to "allowed".
  const canSchedule = status.feasibility?.status === "ok";
  const done = status.vacuum.completed_at;
  const armed = status.vacuum.requested_at;
  const last = status.vacuum.last_result;

  return (
    <>
      <SiteHeader title="Database Settings" />
      <div className="flex flex-col gap-4 p-4 lg:p-6 max-w-3xl">
        {error && (
          <div className="flex items-center justify-between gap-3">
            <p className="text-destructive text-sm">{error}</p>
            <Button variant="outline" size="sm" onClick={refresh}>
              Retry
            </Button>
          </div>
        )}
        <Card>
          <CardHeader>
            <CardTitle>Storage</CardTitle>
            <CardDescription>
              Database size and available disk space.
            </CardDescription>
          </CardHeader>
          <CardContent className="grid gap-2 text-sm">
            <div className="flex justify-between">
              <span className="text-muted-foreground">Backend</span>
              <span>
                <span className="bg-muted rounded px-1 py-0.5 text-[11px] font-mono">
                  {status.backend}
                </span>
              </span>
            </div>
            {status.disk && (
              <>
                <div className="flex justify-between gap-4">
                  <span className="text-muted-foreground">
                    Total database size
                  </span>
                  <AlignedBytes n={status.disk.db_bytes} />
                </div>
                <div className="flex justify-between gap-4">
                  <span className="text-muted-foreground">
                    Write-ahead log size
                  </span>
                  <AlignedBytes n={status.disk.wal_bytes} />
                </div>
                <div className="flex justify-between gap-4">
                  <span className="text-muted-foreground">
                    Free on <Mono>{status.disk.db_path}</Mono>
                  </span>
                  <AlignedBytes n={status.disk.db_fs_free} />
                </div>
                <div className="flex justify-between gap-4">
                  <span className="text-muted-foreground">
                    Free on <Mono>{status.disk.temp_path}</Mono>
                  </span>
                  <AlignedBytes n={status.disk.temp_fs_free} />
                </div>
                <p className="text-xs text-muted-foreground pt-1">
                  {status.disk.same_filesystem
                    ? "The database and temp directories share a filesystem, so a cleanup needs about 2.2x the database size free on it."
                    : "The database and temp directories are on separate filesystems, so a cleanup needs about 1.2x the database size free on each."}
                </p>
              </>
            )}
          </CardContent>
        </Card>

        {sqlite && (
          <Card>
            <CardHeader>
              <CardTitle>Reclaim disk space</CardTitle>
              <CardDescription>
                Deleting records frees space inside the database file but does
                not shrink the file itself. A one-time cleanup rebuilds it,
                returns that space to the disk, and lets future deletes release
                space on their own.
              </CardDescription>
            </CardHeader>
            <CardContent className="flex flex-col gap-3 text-sm">
              <div className="text-muted-foreground">
                <span className="text-foreground font-medium">
                  This only needs doing once.
                </span>{" "}
                Run it if disk usage keeps climbing even as you delete records,
                or if this database has never had a cleanup. Afterwards space is
                released automatically and there is no reason to run it again.
              </div>
              <div className="text-muted-foreground">
                The cleanup runs during startup, before this instance accepts
                any connections, so it will be offline until it finishes. This
                cleanup rewrites the entire database file, which can take a long
                time on a large database.
              </div>
              {last && (
                <div
                  className={
                    last.status === "ok"
                      ? "text-muted-foreground"
                      : "text-destructive"
                  }
                >
                  {last.status === "ok"
                    ? `Last run reclaimed ${bytes(last.reclaimed_bytes)}.`
                    : `Last run failed: ${last.error}`}
                </div>
              )}
              {insufficient && (
                <div className="text-destructive">
                  Not enough free space: needs about{" "}
                  <Mono>{bytes(insufficient.needed)}</Mono> on{" "}
                  <Mono>{insufficient.path}</Mono>, only{" "}
                  <Mono>{bytes(insufficient.available)}</Mono> available.
                </div>
              )}
              {unmeasurable && (
                <div className="text-destructive">
                  Free space on <Mono>{unmeasurable.path}</Mono> could not be
                  measured, so a cleanup cannot be scheduled until this is
                  resolved.
                </div>
              )}
              {!armed && !canSchedule && !insufficient && !unmeasurable && (
                <div className="text-muted-foreground">
                  Disk usage could not be determined for this database, so
                  scheduling is disabled.
                </div>
              )}
              {done && !armed && (
                <div className="text-muted-foreground">
                  This database has already been cleaned up, so it releases
                  space on its own. Running it again would not reclaim anything
                  further.
                </div>
              )}
              {armed ? (
                <div className="flex items-center gap-3">
                  <span>Scheduled; will run on the next restart.</span>
                  <Button variant="outline" onClick={onCancel} disabled={busy}>
                    Cancel
                  </Button>
                </div>
              ) : (
                <div>
                  <Button onClick={onSchedule} disabled={busy || !canSchedule}>
                    Schedule cleanup
                  </Button>
                </div>
              )}
            </CardContent>
          </Card>
        )}
      </div>
    </>
  );
}
