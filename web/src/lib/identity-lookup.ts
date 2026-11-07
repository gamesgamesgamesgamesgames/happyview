import type { ResolvedIdentity } from "@/types/identity";

import { ApiError, resolveIdentity } from "./api";

// Each lookup resolves a DID document and fetches a profile from the account's
// PDS, so lookups run a few at a time instead of all at once for a long list.
export const MAX_IN_FLIGHT = 4;
let inFlight = 0;
let waiting: Array<() => void> = [];

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
export type Unavailable = "unavailable";
export type Lookup = ResolvedIdentity | null | Unavailable;

// One lookup per DID for the life of the page. The resolver answers 400 only
// for a DID that is malformed or has no usable document, which is cached as
// null. Timeouts, rate limits and unreachable hosts (502) and network errors
// are dropped from the cache, so the next component to show that DID retries.
const identities = new Map<string, Promise<Lookup>>();

export function lookupIdentity(did: string): Promise<Lookup> {
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

/** Forgets every cached lookup and queued slot, so each test starts clean. */
export function resetIdentityLookups() {
  identities.clear();
  inFlight = 0;
  waiting = [];
}
