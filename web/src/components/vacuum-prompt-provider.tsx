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
import { toast } from "sonner";

import { getDatabaseStatus, type DatabaseStatus } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { useRestart } from "@/lib/restart-context";

const SEEN_PROMPT = "happyview:vacuum-prompt-seen";
const SEEN_RESULT = "happyview:vacuum-result-seen";
// Mirrors PluginUpdateProvider's poll interval — vacuum state changes rarely
// (armed once, resolved at the next boot), but this is what catches a vacuum
// armed by another admin/session without requiring a full page reload.
const POLL_INTERVAL_MS = 60_000;

function seen(key: string, value: string): boolean {
  if (typeof window === "undefined") return true;
  try {
    return window.localStorage.getItem(key) === value;
  } catch {
    return true;
  }
}

function markSeen(key: string, value: string) {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // ignore
  }
}

interface VacuumStatusContextValue {
  status: DatabaseStatus | null;
  /** Re-fetch immediately — called by the settings page right after a
   * successful schedule/cancel so the restart banner updates without
   * waiting for the next poll. */
  refresh: () => Promise<void>;
}

const VacuumStatusContext = createContext<VacuumStatusContextValue | null>(null);

/**
 * Surfaces two things the user would otherwise have to go looking for: that a
 * one-time vacuum is available on an instance predating incremental vacuum,
 * and — more importantly — how a scheduled vacuum turned out, since it runs
 * at boot when nobody is watching.
 *
 * Also owns the "Restart required" banner reason for a scheduled vacuum.
 * That registration lives here — globally mounted for the whole dashboard
 * session — rather than in the settings page, so it reflects a vacuum armed
 * in an earlier session or by another admin even when the operator never
 * opens Settings -> Database. It re-evaluates on every status refresh: the
 * initial mount fetch, the poll below, and an explicit `refresh()` call from
 * the settings page after scheduling or cancelling.
 */
export function VacuumPromptProvider({ children }: { children: ReactNode }) {
  const router = useRouter();
  const { addReason, removeReason } = useRestart();
  const [status, setStatus] = useState<DatabaseStatus | null>(null);

  const fetchStatus = useCallback(async (): Promise<DatabaseStatus | null> => {
    try {
      return await getDatabaseStatus();
    } catch {
      return null; // best-effort; the user may lack settings:manage
    }
  }, []);

  // Mount + poll. The fetch is issued from a local closure rather than
  // invoking a shared setState-ing callback directly from the effect body,
  // so this stays a subscription to an external system rather than an
  // effect that itself performs the update synchronously.
  useEffect(() => {
    let cancelled = false;

    async function poll() {
      const next = await fetchStatus();
      if (!cancelled && next) setStatus(next);
    }

    poll();
    const id = setInterval(poll, POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [fetchStatus]);

  // Exposed via context for the settings page to call after a successful
  // schedule/cancel, for an immediate (non-polled) update.
  const refresh = useCallback(async () => {
    const next = await fetchStatus();
    if (next) setStatus(next);
  }, [fetchStatus]);

  // Restart banner: reflects live status regardless of which page is open.
  useEffect(() => {
    if (status?.backend === "sqlite" && status.vacuum.requested_at) {
      addReason("vacuum", "A database cleanup is scheduled and will run on the next restart.");
    } else {
      removeReason("vacuum");
    }
  }, [status, addReason, removeReason]);

  // Toasts persist until the user acts on them.
  //
  // These are not "saved!" confirmations — they tell an operator their database
  // needs maintenance, or report the outcome of a rebuild that ran at boot when
  // nobody was watching. A default ~4s toast is long enough to be missed
  // entirely, which defeats the point of announcing it at all. So: no timeout,
  // a close button, and dismissal is what marks it seen.
  //
  // Marking on *dismissal* rather than on display also means reloading before
  // you have engaged with it brings it back, instead of burning the one showing
  // on a moment you were looking elsewhere.
  //
  // The fixed `id`s matter: without them the 60s poll would stack a fresh copy
  // every minute, since we no longer mark seen at display time. Sonner treats a
  // repeated id as an update to the existing toast.
  useEffect(() => {
    if (!status || status.backend !== "sqlite") return;

    const goToSettings = (id: string, key: string, value: string) => {
      markSeen(key, value);
      toast.dismiss(id);
      router.push("/dashboard/settings/database");
    };

    const result = status.vacuum.last_result;
    if (result && !seen(SEEN_RESULT, result.at)) {
      const id = "vacuum-result";
      const common = {
        id,
        duration: Infinity,
        closeButton: true,
        onDismiss: () => markSeen(SEEN_RESULT, result.at),
        action: {
          label: "Review",
          onClick: () => goToSettings(id, SEEN_RESULT, result.at),
        },
      };
      if (result.status === "ok") {
        toast.success("Database cleanup complete", {
          ...common,
          description: `Reclaimed ${formatBytes(result.reclaimed_bytes)}.`,
        });
      } else {
        toast.error("Database cleanup failed", {
          ...common,
          description: result.error ?? "No reason was recorded.",
        });
      }
      return;
    }

    const needsVacuum =
      !status.vacuum.completed_at && !status.vacuum.requested_at;
    if (needsVacuum && !seen(SEEN_PROMPT, "1")) {
      const id = "vacuum-prompt";
      toast("Reclaim unused disk space", {
        id,
        duration: Infinity,
        closeButton: true,
        description:
          "If disk usage keeps climbing even as you delete records, a one-time cleanup will reclaim it. This only needs doing once.",
        onDismiss: () => markSeen(SEEN_PROMPT, "1"),
        action: {
          label: "Review",
          onClick: () => goToSettings(id, SEEN_PROMPT, "1"),
        },
      });
    }
  }, [status, router]);

  const value = useMemo<VacuumStatusContextValue>(
    () => ({ status, refresh }),
    [status, refresh],
  );

  return (
    <VacuumStatusContext.Provider value={value}>{children}</VacuumStatusContext.Provider>
  );
}

export function useVacuumStatus() {
  const ctx = useContext(VacuumStatusContext);
  if (!ctx) {
    throw new Error("useVacuumStatus must be used within VacuumPromptProvider");
  }
  return ctx;
}
