/** `GET /admin/limits`: the instance plan's limits and what is in use. A
 * `max` of `null` is unlimited. */
export type CountedLimit = { max: number | null; used: number };

export type InstanceLimits = {
  seats: CountedLimit;
  api_clients: CountedLimit;
  log_retention_days: { max: number | null; configured: number };
};

export type LimitName = keyof InstanceLimits;

/** A `LimitReached` refusal's fields. */
export type LimitReached = { limit: LimitName; max: number; current: number };
