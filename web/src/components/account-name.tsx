"use client";

import { useEffect, useState } from "react";

import { resolveIdentity } from "@/lib/api";
import { cn } from "@/lib/utils";

// One lookup per DID for the life of the page. A failed or unverified lookup
// caches as null, so the DID is shown instead of retrying on every render.
const handles = new Map<string, Promise<string | null>>();

function lookupHandle(did: string): Promise<string | null> {
  let pending = handles.get(did);
  if (!pending) {
    pending = resolveIdentity(did)
      .then((identity) => identity.handle)
      .catch(() => null);
    handles.set(did, pending);
  }
  return pending;
}

/** The verified handle for a DID: `undefined` while loading, `null` if none. */
export function useHandle(did: string): string | null | undefined {
  const [resolved, setResolved] = useState<{
    did: string;
    handle: string | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    lookupHandle(did).then((handle) => {
      if (!cancelled) setResolved({ did, handle });
    });
    return () => {
      cancelled = true;
    };
  }, [did]);

  return resolved?.did === did ? resolved.handle : undefined;
}

/**
 * An account shown by its handle, with the DID on hover. Falls back to the
 * DID when it has no verified handle. `showDid` adds the DID as a second line.
 */
export function AccountName({
  did,
  showDid = false,
  className,
}: {
  did: string;
  showDid?: boolean;
  className?: string;
}) {
  const handle = useHandle(did);

  if (!handle) {
    return (
      <span className={cn("font-mono text-xs break-all", className)} title={did}>
        {did}
      </span>
    );
  }

  return (
    <span className={cn("inline-flex min-w-0 flex-col", className)} title={did}>
      <span className="truncate text-sm">@{handle}</span>
      {showDid && (
        <span className="text-muted-foreground font-mono text-xs break-all">
          {did}
        </span>
      )}
    </span>
  );
}
