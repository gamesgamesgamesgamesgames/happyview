import type { SettingEntry } from "@/lib/api";

// Every key the Save button writes, in the order it writes them.
export const SAVED_KEYS = [
  "app_name",
  "client_uri",
  "logo_uri",
  "tos_uri",
  "policy_uri",
  "backfill_concurrent_dids_per_pds",
  "backfill_concurrent_pds",
  "backfill_concurrent_resolution",
  "backfill_retention_days",
  "event_log_retention_days",
  "feature.space_inspector_enabled",
  "script_instruction_limit",
  "script_wall_clock_seconds",
  "space_access_log_retention_days",
  "space_inspector_max_grant_minutes",
  "verbose_event_logging",
] as const;

export type FieldKey = (typeof SAVED_KEYS)[number];

export type SettingSource = SettingEntry["source"] | "unset";

export type SettingWrite =
  | { key: FieldKey; action: "put"; value: string }
  | { key: FieldKey; action: "delete" };

/**
 * What saving the form sends. An empty value clears a stored setting so the
 * env var or the default applies, and sends nothing when none is stored. A
 * setting the hosting platform fixes is never sent: the server refuses to
 * change it, and that refusal would stop the rest of the save.
 */
export function settingWrites(
  values: Record<FieldKey, string>,
  sources: Record<FieldKey, SettingSource>,
): SettingWrite[] {
  return SAVED_KEYS.flatMap((key): SettingWrite[] => {
    if (sources[key] === "platform") return [];
    const value = values[key];
    if (value !== "") return [{ key, action: "put", value }];
    return sources[key] === "database" ? [{ key, action: "delete" }] : [];
  });
}
