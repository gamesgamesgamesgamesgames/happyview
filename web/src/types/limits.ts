/** A limit an instance's host can set, by its wire name. */
export type LimitName =
  "seats" | "api_clients" | "anonymous_xrpc" | "log_retention_days";

/**
 * A `LimitReached` refusal: the instance's limits don't allow what was asked.
 * `message` is the host's wording, or HappyView's generic sentence when the
 * host gave none, so it's always there to show. An action comes with both a
 * URL and a label, or not at all.
 */
export interface LimitReached {
  limit: LimitName | string;
  max?: number;
  current?: number;
  message: string;
  actionUrl?: string;
  actionLabel?: string;
}
