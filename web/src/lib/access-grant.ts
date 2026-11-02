import type { AccessGrant, GrantScope } from "@/types/spaces";

const DURATIONS = [5, 15, 30, 60, 120, 240, 480];

/**
 * The lengths a moderator can pick, in minutes: the usual ones up to the
 * instance's maximum, and the maximum itself.
 */
export function grantDurations(maxMinutes: number): number[] {
  return Array.from(
    new Set([...DURATIONS.filter((d) => d <= maxMinutes), maxMinutes]),
  ).sort((a, b) => a - b);
}

export function formatGrantDuration(minutes: number): string {
  return minutes < 60
    ? `${minutes} minutes`
    : `${minutes / 60} hour${minutes === 60 ? "" : "s"}`;
}

/** The body of a grant request, as the dialog's fields hold it. */
export function grantRequest(
  option: { scope: GrantScope; target: string },
  reason: string,
  minutes: string,
) {
  return {
    scope: option.scope,
    target: option.target,
    reason: reason.trim(),
    duration_minutes: Number(minutes),
  };
}

/**
 * The grant a page shows: none while the inspector is off, and none for a
 * target other than the one it was fetched for.
 */
export function grantForTarget(
  found: { key: string; grant: AccessGrant | null } | null,
  targetKey: string,
  enabled: boolean,
): AccessGrant | null {
  return enabled && found?.key === targetKey ? found.grant : null;
}

export function formatRemaining(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")} remaining`;
}
