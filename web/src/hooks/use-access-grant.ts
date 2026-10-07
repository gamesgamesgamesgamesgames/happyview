"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { useCallbackRef } from "@/hooks/use-callback-ref";
import { listAccessGrants, revokeAccessGrant } from "@/lib/api";
import { toastError } from "@/lib/format";
import type { AccessGrant } from "@/types/spaces";

/**
 * The caller's active grant matching `match`, with a countdown. The grant is
 * dropped locally the moment it expires; the server enforces the same expiry.
 * It is hidden (not fetched, not returned) while `enabled` is false.
 *
 * `targetKey` names what the page is showing, such as a space id or a DID. A
 * grant found for one key is never returned for another, and a new key
 * refetches, so navigating between targets can't show the previous target's
 * grant.
 */
export function useAccessGrant(
  targetKey: string,
  match: (grant: AccessGrant) => boolean,
  enabled: boolean,
) {
  const [found, setFound] = useState<{
    key: string;
    grant: AccessGrant | null;
  } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const matchRef = useCallbackRef(match);
  // Bumped by every refresh and every local change. A refresh that resolves
  // after a newer one, or after a grant was set locally, discards its result.
  const generation = useRef(0);

  const refresh = useCallback(async () => {
    if (!enabled) return;
    const current = ++generation.current;
    try {
      const { grants } = await listAccessGrants(true);
      if (current !== generation.current) return;
      const matching = grants
        .filter((g) => matchRef(g))
        .sort((a, b) => Date.parse(b.expires_at) - Date.parse(a.expires_at));
      setFound({ key: targetKey, grant: matching[0] ?? null });
    } catch (e) {
      if (current !== generation.current) return;
      setFound({ key: targetKey, grant: null });
      toastError("Couldn't load access grants", e);
    } finally {
      setNow(Date.now());
    }
  }, [enabled, matchRef, targetKey]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Hidden while disabled, and for any target other than the one it was
  // fetched for.
  const grant =
    enabled && found?.key === targetKey ? found.grant : null;

  useEffect(() => {
    if (!grant) return;
    const timer = setInterval(() => {
      setNow(Date.now());
      if (Date.parse(grant.expires_at) <= Date.now()) {
        setFound((current) =>
          current?.grant?.id === grant.id ? { ...current, grant: null } : current,
        );
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [grant]);

  const remainingMs = grant ? Date.parse(grant.expires_at) - now : 0;

  const setGrant = useCallback(
    (next: AccessGrant | null) => {
      generation.current += 1;
      setFound({ key: targetKey, grant: next });
      setNow(Date.now());
    },
    [targetKey],
  );

  const end = useCallback(async () => {
    if (!grant) return;
    await revokeAccessGrant(grant.id);
    generation.current += 1;
    setFound({ key: targetKey, grant: null });
    // Another grant may still cover the page.
    await refresh();
  }, [grant, refresh, targetKey]);

  const drop = useCallback(() => {
    generation.current += 1;
    setFound({ key: targetKey, grant: null });
  }, [targetKey]);

  return { grant, setGrant, remainingMs, refresh, end, drop };
}
