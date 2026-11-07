"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { getOfficialPlugins, type OfficialPluginSummary } from "@/lib/api";

export function useOfficialPlugins() {
  const [plugins, setPlugins] = useState<OfficialPluginSummary[]>([]);
  const [loading, setLoading] = useState(true);

  const refresh = useCallback(async () => {
    try {
      const res = await getOfficialPlugins();
      setPlugins(res.plugins);
    } catch {
      // swallow — registry may be empty on startup
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 60_000);
    return () => clearInterval(id);
  }, [refresh]);

  const byId = useMemo(() => {
    const map = new Map<string, OfficialPluginSummary>();
    for (const p of plugins) map.set(p.id, p);
    return map;
  }, [plugins]);

  // A retired project is still looked up by id, so an installed plugin can
  // show what became of it, but it is no longer offered to add.
  const listed = useMemo(
    () => plugins.filter((p) => p.lifecycle_state !== "retired"),
    [plugins],
  );

  return { plugins, listed, byId, loading, refresh };
}
