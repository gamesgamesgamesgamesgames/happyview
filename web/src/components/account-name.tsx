"use client";

import { type RefObject, useEffect, useRef, useState } from "react";

import { type Lookup, lookupIdentity } from "@/lib/identity-lookup";
import { cn } from "@/lib/utils";
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";

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
      className={cn(
        "inline-flex min-w-0 items-center gap-2 align-middle",
        className,
      )}
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
