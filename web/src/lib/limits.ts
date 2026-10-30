import type { InstanceLimits, LimitName } from "@/types/limits";

export type LimitRow = {
  limit: LimitName;
  label: string;
  /** "2 of 5", "Up to 7 days" */
  value: string;
  /** Usage or the setting is past the limit: a plan moved down under it. */
  over: boolean;
};

const LABELS: Record<LimitName, string> = {
  seats: "Seats",
  api_clients: "API clients",
  log_retention_days: "Log retention",
};

const days = (n: number) => `${n} ${n === 1 ? "day" : "days"}`;

/** The limits the plan sets, one row each. Unlimited ones are left out, so an
 * instance with no limits gets no rows. */
export function limitRows(limits: InstanceLimits): LimitRow[] {
  const rows: LimitRow[] = [];
  for (const limit of ["seats", "api_clients"] as const) {
    const { max, used } = limits[limit];
    if (max === null) continue;
    rows.push({
      limit,
      label: LABELS[limit],
      value: `${used} of ${max}`,
      over: used > max,
    });
  }
  const retention = limits.log_retention_days;
  if (retention.max !== null) {
    // 0 means "keep forever", which is past any limit.
    const configured = retention.configured;
    rows.push({
      limit: "log_retention_days",
      label: LABELS.log_retention_days,
      value: `Up to ${days(retention.max)}`,
      over: configured === 0 || configured > retention.max,
    });
  }
  return rows;
}

const NOUNS: Record<LimitName, [string, string]> = {
  seats: ["seat", "seats"],
  api_clients: ["API client", "API clients"],
  log_retention_days: ["day of logs", "days of logs"],
};

/** What to tell someone a `LimitReached` stopped. */
export function limitReachedText(limit: LimitName, max: number): string {
  const [one, many] = NOUNS[limit];
  return `This instance's plan allows ${max} ${max === 1 ? one : many}. Remove one, or move the instance to a larger plan.`;
}
