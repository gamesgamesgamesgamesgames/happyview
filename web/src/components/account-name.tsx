"use client";

import { useEffect, useState } from "react";

import { resolveIdentity } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { ResolvedIdentity } from "@/types/identity";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";

// One lookup per DID for the life of the page. A DID that can't be resolved
// caches as null, so it isn't retried on every render.
const identities = new Map<string, Promise<ResolvedIdentity | null>>();

function lookupIdentity(did: string): Promise<ResolvedIdentity | null> {
  let pending = identities.get(did);
  if (!pending) {
    pending = resolveIdentity(did, { profile: true }).catch(() => null);
    identities.set(did, pending);
  }
  return pending;
}

/**
 * A DID's identity: `undefined` while loading, `null` when the DID can't be
 * resolved.
 */
export function useIdentity(did: string): ResolvedIdentity | null | undefined {
  const [resolved, setResolved] = useState<{
    did: string;
    identity: ResolvedIdentity | null;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    lookupIdentity(did).then((identity) => {
      if (!cancelled) setResolved({ did, identity });
    });
    return () => {
      cancelled = true;
    };
  }, [did]);

  return resolved?.did === did ? resolved.identity : undefined;
}

/**
 * An account as avatar, handle and DID, matching the account input's chips.
 * A DID that can't be resolved, or whose handle isn't verified in both
 * directions, shows "Invalid Handle".
 */
export function AccountName({
  did,
  className,
}: {
  did: string;
  className?: string;
}) {
  const identity = useIdentity(did);
  const loading = identity === undefined;
  const handle = identity?.handle;

  return (
    <span
      className={cn("inline-flex min-w-0 items-center gap-2 align-middle", className)}
      title={identity?.display_name ?? undefined}
    >
      <Avatar className="size-6">
        {identity?.avatar && <AvatarImage src={identity.avatar} alt="" />}
        <AvatarFallback className="text-[10px]">
          {handle ? handle.slice(0, 1).toUpperCase() : ""}
        </AvatarFallback>
      </Avatar>
      <span className="flex min-w-0 flex-col items-start leading-tight">
        <span
          className={cn(
            "truncate text-sm",
            loading && "text-muted-foreground",
            !loading && !handle && "text-muted-foreground italic",
          )}
        >
          {loading ? "Resolving…" : handle ? `@${handle}` : "Invalid Handle"}
        </span>
        <span className="text-muted-foreground font-mono text-[10px] break-all">
          {did}
        </span>
      </span>
    </span>
  );
}
