/** The `/config` features the dashboard shows or hides pages by. */
export interface Features {
  spaces: boolean;
  space_inspector: boolean;
}

/**
 * The features `/config` reports. Anything but an explicit `true` is off, so
 * a server that predates a feature reads as not having it.
 */
export function parseFeatures(raw: unknown): Features {
  const features =
    raw && typeof raw === "object" ? (raw as Record<string, unknown>) : {};
  return {
    spaces: features.spaces === true,
    space_inspector: features.space_inspector === true,
  };
}

/** Why the dashboard could not load `/config`. */
export type ConfigFailure =
  | {
      /** The server answered 421: it does not serve this host name. */
      kind: "unknown-host";
      host: string;
      /** The host is `localhost`, which local development should not use. */
      isLocalhost: boolean;
    }
  | {
      /** No answer, or an error answer: the server may be starting or down. */
      kind: "unreachable";
      detail: string;
    };

/**
 * What a failed `/config` request means. `status` is null when the request
 * got no response at all.
 */
export function classifyConfigFailure(
  status: number | null,
  host: string,
  detail: string,
): ConfigFailure {
  if (status === 421) {
    const hostname = host.split(":")[0].toLowerCase();
    return {
      kind: "unknown-host",
      host,
      isLocalhost: hostname === "localhost",
    };
  }
  return { kind: "unreachable", detail };
}

const FIRST_RETRY_MS = 1_000;
const MAX_RETRY_MS = 30_000;

/** How long to wait before retry number `attempt` (0 for the first). */
export function retryDelayMs(attempt: number): number {
  return Math.min(FIRST_RETRY_MS * 2 ** Math.max(0, attempt), MAX_RETRY_MS);
}
