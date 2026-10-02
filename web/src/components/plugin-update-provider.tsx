"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { useRouter } from "next/navigation";
import semver from "semver";
import { toast } from "sonner";
import { getPlugins, type PluginSummary } from "@/lib/api";

interface PluginUpdatesContextValue {
  plugins: PluginSummary[];
  hasUpdates: boolean;
  refresh: () => Promise<void>;
  markSeen: (id: string, version: string) => void;
}

const PluginUpdatesContext = createContext<PluginUpdatesContextValue | null>(
  null,
);

const STORAGE_PREFIX = "happyview:plugin-update-seen:";

function storageKey(id: string) {
  return `${STORAGE_PREFIX}${id}`;
}

function readSeen(id: string): string | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage.getItem(storageKey(id));
  } catch {
    return null;
  }
}

function writeSeen(id: string, version: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(storageKey(id), version);
  } catch {
    // ignore
  }
}

function isNewerThanSeen(latest: string, seen: string | null): boolean {
  if (!seen) return true;
  const a = semver.coerce(latest);
  const b = semver.coerce(seen);
  if (!a || !b) return latest !== seen;
  return semver.gt(a, b);
}

export function PluginUpdateProvider({ children }: { children: ReactNode }) {
  const router = useRouter();
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);

  const refresh = useCallback(() => {
    return getPlugins()
      .then((res) => setPlugins(res.plugins))
      .catch(() => {
        // ignore — dashboard may not be ready
      });
  }, []);

  const markSeen = useCallback((id: string, version: string) => {
    writeSeen(id, version);
  }, []);

  useEffect(() => {
    refresh();
    const id = setInterval(refresh, 60_000);
    return () => clearInterval(id);
  }, [refresh]);

  useEffect(() => {
    for (const p of plugins) {
      if (!p.update_available || !p.latest_version) continue;
      const seen = readSeen(p.id);
      if (!isNewerThanSeen(p.latest_version, seen)) continue;
      toast(`${p.name} v${p.latest_version} is available`, {
        action: {
          label: "Review",
          onClick: () =>
            router.push(
              `/dashboard/settings/plugins?update=${encodeURIComponent(p.id)}`,
            ),
        },
      });
      writeSeen(p.id, p.latest_version);
    }
  }, [plugins, router]);

  const value = useMemo<PluginUpdatesContextValue>(
    () => ({
      plugins,
      hasUpdates: plugins.some((p) => p.update_available),
      refresh,
      markSeen,
    }),
    [plugins, refresh, markSeen],
  );

  return (
    <PluginUpdatesContext.Provider value={value}>
      {children}
    </PluginUpdatesContext.Provider>
  );
}

export function usePluginUpdates() {
  const ctx = useContext(PluginUpdatesContext);
  if (!ctx) {
    throw new Error(
      "usePluginUpdates must be used within PluginUpdateProvider",
    );
  }
  return ctx;
}
