"use client";

import { useCallback, useEffect, useState } from "react";

import { useCallbackRef } from "@/hooks/use-callback-ref";
import { listAccessGrants, revokeAccessGrant } from "@/lib/api";
import type { AccessGrant } from "@/types/spaces";

/**
 * The caller's active grant matching `match`, with a countdown. The grant is
 * dropped locally the moment it expires; the server enforces the same expiry.
 * It is hidden (not fetched, not returned) while `enabled` is false.
 */
export function useAccessGrant(
  match: (grant: AccessGrant) => boolean,
  enabled: boolean,
) {
  const [grant, setGrant] = useState<AccessGrant | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const matchRef = useCallbackRef(match);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    try {
      const { grants } = await listAccessGrants(true);
      const matching = grants
        .filter((g) => matchRef(g))
        .sort((a, b) => Date.parse(b.expires_at) - Date.parse(a.expires_at));
      setGrant(matching[0] ?? null);
    } catch {
      setGrant(null);
    } finally {
      setNow(Date.now());
    }
  }, [enabled, matchRef]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    if (!enabled || !grant) return;
    const timer = setInterval(() => {
      setNow(Date.now());
      if (Date.parse(grant.expires_at) <= Date.now()) setGrant(null);
    }, 1000);
    return () => clearInterval(timer);
  }, [enabled, grant]);

  // Hidden while disabled, even if the last fetch is still cached.
  const visibleGrant = enabled ? grant : null;
  const remainingMs = visibleGrant
    ? Date.parse(visibleGrant.expires_at) - now
    : 0;

  const end = useCallback(async () => {
    if (!grant) return;
    await revokeAccessGrant(grant.id);
    setGrant(null);
  }, [grant]);

  const drop = useCallback(() => setGrant(null), []);

  return { grant: visibleGrant, setGrant, remainingMs, refresh, end, drop };
}
