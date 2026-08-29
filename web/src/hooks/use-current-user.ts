import { useCallback, useEffect, useState } from "react";

import { useAuth } from "@/lib/auth-context";
import { getUsers } from "@/lib/api";
import type { UserSummary } from "@/types/users";

export function useCurrentUser() {
  const { did } = useAuth();
  const [{ fetchedFor, user }, setResult] = useState<{
    fetchedFor: string | null | undefined;
    user: UserSummary | null;
  }>({ fetchedFor: undefined, user: null });
  const [refreshKey, setRefreshKey] = useState(0);

  useEffect(() => {
    let cancelled = false;
    getUsers()
      .then((users) => {
        if (cancelled) return;
        setResult({
          fetchedFor: did,
          user: users.find((u) => u.did === did) ?? null,
        });
      })
      .catch(() => {
        if (cancelled) return;
        setResult({ fetchedFor: did, user: null });
      });
    return () => {
      cancelled = true;
    };
  }, [did, refreshKey]);

  const loading = fetchedFor === undefined || fetchedFor !== did;
  const currentUser = user;

  const isSuper = currentUser?.is_super ?? false;

  const hasPermission = useCallback(
    (permission: string) =>
      isSuper || (currentUser?.permissions.includes(permission) ?? false),
    [currentUser, isSuper],
  );

  const reload = useCallback(() => setRefreshKey((k) => k + 1), []);

  return { currentUser, isSuper, hasPermission, loading, reload };
}
