import type { LimitReached } from "@/types/limits";

/**
 * A `LimitReached` refusal from a parsed error body, or undefined for any
 * other error. The action is kept only when it has both halves and the URL
 * is http(s): it comes from the instance's host, and a toast button must not
 * run a `javascript:` URL.
 */
export function parseLimitReached(body: unknown): LimitReached | undefined {
  if (!body || typeof body !== "object") return undefined;
  const b = body as Record<string, unknown>;
  if (b.error !== "LimitReached" || typeof b.limit !== "string") {
    return undefined;
  }
  const message =
    typeof b.message === "string" && b.message.trim()
      ? b.message.trim()
      : "This instance's limits don't allow that.";
  const reached: LimitReached = { limit: b.limit, message };
  if (typeof b.max === "number") reached.max = b.max;
  if (typeof b.current === "number") reached.current = b.current;
  const url = typeof b.actionUrl === "string" ? b.actionUrl.trim() : "";
  const label = typeof b.actionLabel === "string" ? b.actionLabel.trim() : "";
  if (url && label && isWebUrl(url)) {
    reached.actionUrl = url;
    reached.actionLabel = label;
  }
  return reached;
}

function isWebUrl(url: string): boolean {
  try {
    const { protocol } = new URL(url);
    return protocol === "https:" || protocol === "http:";
  } catch {
    return false;
  }
}

/** The toast for a refusal: what was refused, the host's words, and its action. */
export function limitToast(
  context: string,
  reached: LimitReached,
): {
  title: string;
  description: string;
  action?: { label: string; url: string };
} {
  return {
    title: `${context}: limit reached`,
    description: reached.message,
    action:
      reached.actionUrl && reached.actionLabel
        ? { label: reached.actionLabel, url: reached.actionUrl }
        : undefined,
  };
}
