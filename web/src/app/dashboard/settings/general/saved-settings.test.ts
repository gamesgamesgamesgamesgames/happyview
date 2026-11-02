import { describe, expect, it } from "vitest";

import {
  type FieldKey,
  SAVED_KEYS,
  type SettingSource,
  settingWrites,
} from "./saved-settings";

function form(
  values: Partial<Record<FieldKey, string>>,
  sources: Partial<Record<FieldKey, SettingSource>> = {},
) {
  const allValues = Object.fromEntries(
    SAVED_KEYS.map((key) => [key, values[key] ?? ""]),
  ) as Record<FieldKey, string>;
  const allSources = Object.fromEntries(
    SAVED_KEYS.map((key) => [key, sources[key] ?? "unset"]),
  ) as Record<FieldKey, SettingSource>;
  return settingWrites(allValues, allSources);
}

describe("settingWrites", () => {
  it("writes the inspector switch and its maximum access length", () => {
    expect(
      form({
        "feature.space_inspector_enabled": "true",
        space_inspector_max_grant_minutes: "120",
      }),
    ).toEqual([
      {
        key: "feature.space_inspector_enabled",
        action: "put",
        value: "true",
      },
      {
        key: "space_inspector_max_grant_minutes",
        action: "put",
        value: "120",
      },
    ]);
  });

  it("writes protected retention under its own key", () => {
    expect(form({ space_access_log_retention_days: "0" })).toEqual([
      { key: "space_access_log_retention_days", action: "put", value: "0" },
    ]);
  });

  // Saving 365 for a blank field would stop protected retention following
  // the event log retention.
  it("leaves blank protected retention unset", () => {
    expect(
      form(
        { event_log_retention_days: "0" },
        { event_log_retention_days: "env" },
      ),
    ).toEqual([{ key: "event_log_retention_days", action: "put", value: "0" }]);
  });

  it("clears a stored setting that was blanked", () => {
    expect(form({}, { space_access_log_retention_days: "database" })).toEqual([
      { key: "space_access_log_retention_days", action: "delete" },
    ]);
  });
});
