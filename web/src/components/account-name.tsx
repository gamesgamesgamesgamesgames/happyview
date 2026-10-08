"use client";

import { type RefObject, useEffect, useRef, useState } from "react";

import { ApiError, resolveIdentity } from "@/lib/api";
import { cn } from "@/lib/utils";
import type { ResolvedIdentity } from "@/types/identity";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";

// Each lookup resolves a DID document and fetches a profile from the account's
// PDS, so lookups run a few at a time instead of all at once for a long list.
const MAX_IN_FLIGHT = 4;
let inFlight = 0;
const waiting: Array<() => void> = [];

function whenSlotFree(): Promise<void> {
  if (inFlight < MAX_IN_FLIGHT) {
    inFlight += 1;
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    waiting.push(() => {
      inFlight += 1;
      resolve();
    });
  });
}

function releaseSlot() {
  inFlight -= 1;
  waiting.shift()?.();
}

// A lookup that failed for a reason other than the DID being unresolvable.
type Unavailable = "unavailable";
type Lookup = ResolvedIdentity | null | Unavailable;

// One lookup per DID for the life of the page. A DID the server can't resolve
// (a 400) caches as null. Any other failure is dropped from the cache, so the
// next component to show that DID tries again.
const identities = new Map<string, Promise<Lookup>>();

function lookupIdentity(did: string): Promise<Lookup> {
  let pending = identities.get(did);
  if (!pending) {
    pending = whenSlotFree().then(() =>
      resolveIdentity(did, { profile: true })
        .then((identity): Lookup => identity)
        .catch((e: unknown): Lookup => {
          if (e instanceof ApiError && e.status === 400) return null;
          identities.delete(did);
          return "unavailable";
        })
        .finally(releaseSlot),
    );
    identities.set(did, pending);
  }
  return pending;
}

/**
 * A DID's identity: `undefined` while loading, `null` when the DID can't be
 * resolved, `"unavailable"` when the lookup failed for another reason. With
 * `ref`, the lookup waits until that element is on screen.
 */
export function useIdentity(
  did: string,
  ref?: RefObject<Element | null>,
): Lookup | undefined {
  const [resolved, setResolved] = useState<{
    did: string;
    identity: Lookup;
  } | null>(null);

  useEffect(() => {
    let cancelled = false;
    const start = () =>
      lookupIdentity(did).then((identity) => {
        if (!cancelled) setResolved({ did, identity });
      });

    const element = ref?.current;
    if (!element || typeof IntersectionObserver === "undefined") {
      start();
      return () => {
        cancelled = true;
      };
    }

    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer.disconnect();
        start();
      }
    });
    observer.observe(element);
    return () => {
      cancelled = true;
      observer.disconnect();
    };
  }, [did, ref]);

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
  const ref = useRef<HTMLSpanElement>(null);
  const lookup = useIdentity(did, ref);
  const loading = lookup === undefined;
  const unavailable = lookup === "unavailable";
  const identity = typeof lookup === "object" ? lookup : null;
  const handle = identity?.handle;
  const label = loading
    ? "Resolving…"
    : handle
      ? `@${handle}`
      : unavailable
        ? "Handle unavailable"
        : "Invalid Handle";

  return (
    <span
      ref={ref}
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
          {label}
        </span>
        <span className="text-muted-foreground font-mono text-[10px] break-all">
          {did}
        </span>
      </span>
    </span>
  );
}
