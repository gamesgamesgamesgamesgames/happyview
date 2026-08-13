"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Upload, Trash2 } from "lucide-react";
import Link from "next/link";

import { useCurrentUser } from "@/hooks/use-current-user";
import {
  getSettings,
  getDbInfo,
  upsertSetting,
  deleteSetting,
  uploadLogo,
  deleteLogo,
  countEvents,
  purgeEvents,
  type SettingEntry,
  type DbInfo,
  type EventPurgeFilter,
} from "@/lib/api";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";

const SETTING_KEYS = [
  "app_name",
  "backfill_concurrent_dids_per_pds",
  "backfill_concurrent_pds",
  "backfill_concurrent_resolution",
  "backfill_retention_days",
  "client_uri",
  "event_log_retention_days",
  "logo_uri",
  "tos_uri",
  "policy_uri",
  "verbose_event_logging",
] as const;

type FieldKey = (typeof SETTING_KEYS)[number];

type FieldConfig = {
  key: FieldKey;
  label: string;
  placeholder: string;
  description: string;
};

const FIELDS: FieldConfig[] = [
  {
    key: "app_name",
    label: "Instance Name",
    placeholder: "My HappyView Instance",
    description:
      "Display name for this instance. Shown in the sidebar and on the OAuth consent screen.",
  },
  {
    key: "client_uri",
    label: "Instance URI",
    placeholder: "https://example.com",
    description:
      "The public URL for this instance, linked from the OAuth consent screen.",
  },
  {
    key: "logo_uri",
    label: "Logo URI",
    placeholder: "https://example.com/logo.png",
    description:
      "External URL to a logo image. Overridden by an uploaded logo below.",
  },
  {
    key: "tos_uri",
    label: "Terms of Service URI",
    placeholder: "https://example.com/terms",
    description: "Link to your terms of service. Optional.",
  },
  {
    key: "policy_uri",
    label: "Privacy Policy URI",
    placeholder: "https://example.com/privacy",
    description: "Link to your privacy policy. Optional.",
  },
];

export default function GeneralSettingsPage() {
  const { hasPermission } = useCurrentUser();
  const canManage = hasPermission("settings:manage");
  const canReadEvents = hasPermission("events:read");
  const canPurgeEvents = hasPermission("events:purge");

  const [values, setValues] = useState<Record<FieldKey, string>>({
    app_name: "",
    backfill_concurrent_dids_per_pds: "3",
    backfill_concurrent_pds: "10",
    backfill_concurrent_resolution: "100",
    backfill_retention_days: "28",
    client_uri: "",
    event_log_retention_days: "30",
    logo_uri: "",
    tos_uri: "",
    policy_uri: "",
    verbose_event_logging: "",
  });
  const [sources, setSources] = useState<
    Record<FieldKey, "database" | "env" | "unset">
  >({
    app_name: "unset",
    backfill_concurrent_dids_per_pds: "unset",
    backfill_concurrent_pds: "unset",
    backfill_concurrent_resolution: "unset",
    backfill_retention_days: "unset",
    client_uri: "unset",
    event_log_retention_days: "unset",
    logo_uri: "unset",
    tos_uri: "unset",
    policy_uri: "unset",
    verbose_event_logging: "unset",
  });
  const [logoUploaded, setLogoUploaded] = useState(false);
  const [dbInfo, setDbInfo] = useState<DbInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const [purgeFilter, setPurgeFilter] = useState<EventPurgeFilter>({});
  const [purgeCount, setPurgeCount] = useState<number | null>(null);
  const [purgeBusy, setPurgeBusy] = useState(false);
  const [purgeJobId, setPurgeJobId] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const entries = await getSettings();
      const byKey = new Map<string, SettingEntry>(
        entries.map((e) => [e.key, e]),
      );
      const val = (key: string, fallback: string) =>
        byKey.get(key)?.value ?? fallback;
      const src = (key: string) =>
        (byKey.get(key)?.source as "database" | "env" | undefined) ?? "unset";
      setValues({
        app_name: val("app_name", ""),
        backfill_concurrent_dids_per_pds: val(
          "backfill_concurrent_dids_per_pds",
          "3",
        ),
        backfill_concurrent_pds: val("backfill_concurrent_pds", "10"),
        backfill_concurrent_resolution: val(
          "backfill_concurrent_resolution",
          "100",
        ),
        backfill_retention_days: val("backfill_retention_days", "28"),
        client_uri: val("client_uri", ""),
        event_log_retention_days: val("event_log_retention_days", "30"),
        logo_uri: val("logo_uri", ""),
        tos_uri: val("tos_uri", ""),
        policy_uri: val("policy_uri", ""),
        verbose_event_logging: val("verbose_event_logging", ""),
      });
      setSources({
        app_name: src("app_name"),
        backfill_concurrent_dids_per_pds: src(
          "backfill_concurrent_dids_per_pds",
        ),
        backfill_concurrent_pds: src("backfill_concurrent_pds"),
        backfill_concurrent_resolution: src("backfill_concurrent_resolution"),
        backfill_retention_days: src("backfill_retention_days"),
        client_uri: src("client_uri"),
        event_log_retention_days: src("event_log_retention_days"),
        logo_uri: src("logo_uri"),
        tos_uri: src("tos_uri"),
        policy_uri: src("policy_uri"),
        verbose_event_logging: src("verbose_event_logging"),
      });
      setLogoUploaded(byKey.has("logo_data"));
      try {
        setDbInfo(await getDbInfo());
      } catch {
        // non-critical
      }
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function handleSave() {
    setError(null);
    setNotice(null);
    setSaving(true);
    try {
      for (const field of FIELDS) {
        const value = values[field.key];
        if (value === "") {
          if (sources[field.key] === "database") {
            await deleteSetting(field.key);
          }
        } else {
          await upsertSetting(field.key, value);
        }
      }
      const extraKeys = [
        "backfill_concurrent_dids_per_pds",
        "backfill_concurrent_pds",
        "backfill_concurrent_resolution",
        "backfill_retention_days",
        "event_log_retention_days",
        "verbose_event_logging",
      ] as const;
      for (const key of extraKeys) {
        const value = values[key];
        if (value === "") {
          if (sources[key] === "database") {
            await deleteSetting(key);
          }
        } else {
          await upsertSetting(key, value);
        }
      }
      setNotice("Settings saved.");
      await load();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  async function handleLogoUpload(e: React.ChangeEvent<HTMLInputElement>) {
    const file = e.target.files?.[0];
    if (!file) return;
    setError(null);
    try {
      await uploadLogo(file);
      setNotice("Logo uploaded.");
      await load();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  }

  async function handleLogoDelete() {
    setError(null);
    try {
      await deleteLogo();
      setNotice("Logo removed.");
      await load();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  // Editing any filter invalidates the count, which re-disables Purge. The
  // count is a precondition, not a convenience: this page has no events table
  // in view, so it is the only thing between a mistyped bound and a mass delete.
  function updatePurgeFilter(key: keyof EventPurgeFilter, value: string) {
    setPurgeFilter((prev) => ({ ...prev, [key]: value }));
    setPurgeCount(null);
    setPurgeJobId(null);
  }

  async function handleCheckMatches() {
    setError(null);
    setPurgeBusy(true);
    const requested = purgeFilter;
    try {
      const { count } = await countEvents(requested);
      // Discard a stale resolution: if the filter changed while this request
      // was in flight, `purgeCount` must stay null rather than showing a count
      // computed for a filter that is no longer the one Purge would send.
      setPurgeFilter((cur) => {
        if (cur === requested) setPurgeCount(count);
        return cur;
      });
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPurgeBusy(false);
    }
  }

  async function handlePurge() {
    setError(null);
    setPurgeBusy(true);
    try {
      const { job_id } = await purgeEvents(purgeFilter);
      setPurgeJobId(job_id);
      setPurgeCount(null);
      setNotice("Purge started.");
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPurgeBusy(false);
    }
  }

  const connectionEstimate = useMemo(() => {
    const pds = parseInt(values.backfill_concurrent_pds) || 10;
    const dids = parseInt(values.backfill_concurrent_dids_per_pds) || 3;
    const resolution = parseInt(values.backfill_concurrent_resolution) || 100;
    const needed = pds * dids + resolution + 4;
    const mainPool = dbInfo?.main_pool_size ?? 32;
    const total = needed + mainPool;
    const serverMax = dbInfo?.server_max_connections ?? null;
    return { needed, mainPool, total, serverMax };
  }, [values, dbInfo]);

  const connectionWarning = useMemo(() => {
    if (!connectionEstimate.serverMax) return null;
    if (connectionEstimate.total > connectionEstimate.serverMax) {
      return `These settings need ~${connectionEstimate.total} connections (${connectionEstimate.needed} backfill + ${connectionEstimate.mainPool} main), but the database allows ${connectionEstimate.serverMax}. Reduce concurrency or increase the database's max_connections.`;
    }
    return null;
  }, [connectionEstimate]);

  return (
    <>
      <SiteHeader title="General Settings" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6 max-w-3xl">
        {error && <p className="text-destructive text-sm">{error}</p>}
        {notice && (
          <p className="text-sm text-green-600 dark:text-green-400">{notice}</p>
        )}

        <div>
          <h2 className="text-lg font-semibold">Instance Identity</h2>
          <p className="text-muted-foreground text-sm">
            Configure your HappyView instance. These values are used in the
            dashboard sidebar and on the OAuth consent screen.
          </p>
        </div>

        {FIELDS.map((field) => (
          <div key={field.key} className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <Label htmlFor={field.key}>{field.label}</Label>
              {sources[field.key] === "env" && (
                <span className="text-xs text-muted-foreground">
                  from env var
                </span>
              )}
            </div>
            <Input
              id={field.key}
              value={values[field.key]}
              onChange={(e) =>
                setValues((v) => ({ ...v, [field.key]: e.target.value }))
              }
              placeholder={field.placeholder}
              disabled={!canManage}
            />
            <p className="text-muted-foreground text-xs">{field.description}</p>
          </div>
        ))}

        <div className="flex flex-col gap-2">
          <Label>Logo Upload</Label>
          <p className="text-muted-foreground text-xs">
            Upload a logo (max 5MB). Overrides the Logo URI above when set.
          </p>
          <div className="flex items-center gap-2">
            <input
              ref={fileInputRef}
              type="file"
              accept="image/*"
              className="hidden"
              onChange={handleLogoUpload}
              disabled={!canManage}
            />
            <Button
              type="button"
              variant="outline"
              onClick={() => fileInputRef.current?.click()}
              disabled={!canManage}
            >
              <Upload className="size-4 mr-2" />
              {logoUploaded ? "Replace logo" : "Upload logo"}
            </Button>
            {logoUploaded && (
              <Button
                type="button"
                variant="ghost"
                onClick={handleLogoDelete}
                disabled={!canManage}
              >
                <Trash2 className="size-4 mr-2" />
                Remove
              </Button>
            )}
            {logoUploaded && (
              <span className="text-xs text-muted-foreground">
                Current logo served at /settings/logo
              </span>
            )}
          </div>
        </div>

        <div>
          <h2 className="text-lg font-semibold">Data Retention</h2>
          <p className="text-muted-foreground text-sm">
            Configure how long HappyView retains detailed data from completed
            backfill jobs.
          </p>
        </div>

        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between">
            <Label htmlFor="backfill_retention_days">
              Backfill Detail Retention (days)
            </Label>
            {sources["backfill_retention_days"] === "env" && (
              <span className="text-xs text-muted-foreground">
                from env var
              </span>
            )}
          </div>
          <Input
            id="backfill_retention_days"
            type="number"
            min={0}
            step={1}
            value={values["backfill_retention_days"]}
            onChange={(e) =>
              setValues((v) => ({
                ...v,
                backfill_retention_days: e.target.value,
              }))
            }
            placeholder="28"
            disabled={!canManage}
          />
          <p className="text-muted-foreground text-xs">
            How long to keep per-repo detail data from completed backfill jobs.
            Set to 0 to keep indefinitely.
          </p>
        </div>

        <div>
          <h2 className="text-lg font-semibold">Backfill Performance</h2>
          <p className="text-muted-foreground text-sm">
            Tune concurrency limits for backfill jobs. Changes only apply to new
            or resumed jobs.
          </p>
          {dbInfo?.server_max_connections && (
            <p className="text-muted-foreground text-xs mt-1">
              Database limit: {dbInfo.server_max_connections} connections · Main
              pool: {connectionEstimate.mainPool} · Backfill estimate:{" "}
              {connectionEstimate.needed}
            </p>
          )}
          {connectionWarning && (
            <p className="text-sm text-destructive mt-2">{connectionWarning}</p>
          )}
        </div>

        {[
          {
            key: "backfill_concurrent_resolution" as const,
            id: "backfill_concurrent_resolution",
            label: "Concurrent PLC Resolutions",
            placeholder: "100",
            description:
              "How many DID document lookups to run in parallel during PDS resolution.",
          },
          {
            key: "backfill_concurrent_pds" as const,
            id: "backfill_concurrent_pds",
            label: "Concurrent PDS Hosts",
            placeholder: "10",
            description:
              "How many PDS servers to fetch records from simultaneously.",
          },
          {
            key: "backfill_concurrent_dids_per_pds" as const,
            id: "backfill_concurrent_dids_per_pds",
            label: "Concurrent DIDs per PDS",
            placeholder: "3",
            description:
              "How many repos to fetch concurrently from each PDS host.",
          },
        ].map((field) => (
          <div key={field.key} className="flex flex-col gap-2">
            <div className="flex items-center justify-between">
              <Label htmlFor={field.id}>{field.label}</Label>
              {sources[field.key] === "env" && (
                <span className="text-xs text-muted-foreground">
                  from env var
                </span>
              )}
            </div>
            <Input
              id={field.id}
              type="number"
              min={1}
              step={1}
              value={values[field.key]}
              onChange={(e) =>
                setValues((v) => ({ ...v, [field.key]: e.target.value }))
              }
              placeholder={field.placeholder}
              disabled={!canManage}
            />
            <p className="text-muted-foreground text-xs">{field.description}</p>
          </div>
        ))}

        <div>
          <h2 className="text-lg font-semibold">Event Logs</h2>
          <p className="text-muted-foreground text-sm">
            Configure event log verbosity and retention.
          </p>
        </div>

        <div className="flex flex-col gap-2">
          <div className="flex items-center justify-between">
            <Label htmlFor="event_log_retention_days">
              Event Log Retention (days)
            </Label>
            {sources["event_log_retention_days"] === "env" && (
              <span className="text-xs text-muted-foreground">
                from env var
              </span>
            )}
          </div>
          <Input
            id="event_log_retention_days"
            type="number"
            min={0}
            step={1}
            value={values["event_log_retention_days"]}
            onChange={(e) =>
              setValues((v) => ({
                ...v,
                event_log_retention_days: e.target.value,
              }))
            }
            placeholder="30"
            disabled={!canManage}
          />
          <p className="text-muted-foreground text-xs">
            Delete event log entries older than this many days. 0 disables
            automatic cleanup. Changes take effect within an hour.
          </p>
        </div>

        <div className="flex items-center justify-between">
          <div className="flex flex-col gap-1">
            <div className="flex items-center gap-2">
              <Label htmlFor="verbose_event_logging">
                Verbose Event Logging
              </Label>
              {sources["verbose_event_logging"] === "env" && (
                <span className="text-xs text-muted-foreground">
                  from env var
                </span>
              )}
            </div>
            <p className="text-muted-foreground text-xs">
              Log every record index, hook execution, and hook skip to the event
              log. Generates high write volume and <em>will</em> cause issues if
              you're indexing high-traffic collections. Recommended to only use
              for debugging.
            </p>
          </div>
          <Switch
            id="verbose_event_logging"
            checked={values.verbose_event_logging.toLowerCase() === "true"}
            onCheckedChange={(checked) =>
              setValues((v) => ({
                ...v,
                verbose_event_logging: checked ? "true" : "false",
              }))
            }
            disabled={!canManage}
          />
        </div>

        <div className="flex justify-end pt-2">
          <Button
            onClick={handleSave}
            disabled={!canManage || saving || !!connectionWarning}
          >
            {saving ? "Saving..." : "Save changes"}
          </Button>
        </div>

        {(canReadEvents || canPurgeEvents) && (
          <section className="space-y-4 border-t pt-6">
            <div>
              <h2 className="text-lg font-medium">Purge Event Logs</h2>
              <p className="text-muted-foreground text-sm">
                Delete event log entries matching a filter. Leave every field
                blank to match all entries. Runs as a background job you can
                pause or cancel.
              </p>
            </div>

            <div className="grid gap-4 sm:grid-cols-2">
              {(
                [
                  ["event_type", "Event Type", "record.skipped"],
                  ["category", "Category", "record,script"],
                  ["severity", "Severity", "info,warn,error"],
                  ["subject", "Subject contains", "did:plc:..."],
                  ["after", "After (RFC3339)", "2026-07-01T00:00:00Z"],
                  ["before", "Before (RFC3339)", "2026-08-01T00:00:00Z"],
                ] as const
              ).map(([key, label, placeholder]) => (
                <div key={key} className="space-y-2">
                  <Label htmlFor={`purge-${key}`}>{label}</Label>
                  <Input
                    id={`purge-${key}`}
                    value={purgeFilter[key] ?? ""}
                    placeholder={placeholder}
                    onChange={(e) => updatePurgeFilter(key, e.target.value)}
                    disabled={!canPurgeEvents || purgeBusy}
                  />
                </div>
              ))}
            </div>

            <div className="flex items-center gap-3">
              <Button
                variant="outline"
                onClick={handleCheckMatches}
                disabled={!canReadEvents || purgeBusy}
              >
                Check matches
              </Button>
              <Button
                variant="destructive"
                onClick={handlePurge}
                disabled={
                  !canPurgeEvents ||
                  purgeBusy ||
                  purgeCount === null ||
                  purgeCount === 0
                }
              >
                Purge
              </Button>
              {purgeCount !== null && (
                <span className="text-sm">
                  This will delete {purgeCount.toLocaleString()} events.
                </span>
              )}
              {purgeJobId && (
                <Link
                  href="/dashboard/jobs"
                  className="text-sm underline underline-offset-4"
                >
                  View job progress
                </Link>
              )}
            </div>
          </section>
        )}
      </div>
    </>
  );
}
