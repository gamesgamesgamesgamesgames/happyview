"use client";

import { useEffect, useState } from "react";

import { AlertOctagon, Info } from "lucide-react";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import {
  getUpdateStatus,
  type HappyViewOffer,
  type UpdateStatus,
} from "@/lib/api";

/** The server checks every six hours; the dashboard asks it this often. */
const POLL_MS = 30 * 60 * 1000;

/** How long a security notice stays dismissed. A routine one stays
 * dismissed until a newer version comes out. */
const SECURITY_SNOOZE_MS = 24 * 60 * 60 * 1000;

const DISMISSED_KEY = "happyview:update-dismissed";

interface Dismissal {
  version: string;
  at: number;
}

function readDismissal(): Dismissal | null {
  if (typeof window === "undefined") return null;
  try {
    const raw = window.localStorage.getItem(DISMISSED_KEY);
    return raw ? (JSON.parse(raw) as Dismissal) : null;
  } catch {
    return null;
  }
}

function writeDismissal(d: Dismissal) {
  try {
    window.localStorage.setItem(DISMISSED_KEY, JSON.stringify(d));
  } catch {
    // ignore
  }
}

function isDismissed(offer: HappyViewOffer, d: Dismissal | null): boolean {
  if (!d || d.version !== offer.version) return false;
  if (offer.severity === "security") {
    return Date.now() - d.at < SECURITY_SNOOZE_MS;
  }
  return true;
}

/** The update check's answer, polled while the dashboard is open. */
export function useUpdateStatus(): UpdateStatus | null {
  const [status, setStatus] = useState<UpdateStatus | null>(null);
  useEffect(() => {
    let cancelled = false;
    const load = () =>
      getUpdateStatus()
        .then((s) => {
          if (!cancelled) setStatus(s);
        })
        .catch(() => {
          // An older server, or none reachable: say nothing.
        });
    load();
    const id = setInterval(load, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, []);
  return status;
}

/** Links to the release notes and, for a self-hosted instance, the guide. */
export function OfferLinks({
  offer,
  managed,
}: {
  offer: HappyViewOffer;
  managed: boolean;
}) {
  return (
    <span className="flex flex-wrap gap-x-4 gap-y-1">
      {offer.releaseUrl ? (
        <a
          href={offer.releaseUrl}
          target="_blank"
          rel="noreferrer noopener"
          className="underline underline-offset-4"
        >
          Release notes
        </a>
      ) : null}
      {managed ? null : (
        <a
          href={offer.upgradeGuideUrl}
          target="_blank"
          rel="noreferrer noopener"
          className="underline underline-offset-4"
        >
          How to upgrade
        </a>
      )}
    </span>
  );
}

/** The notice box: muted for a routine release, red for a security one. */
function Notice({
  security,
  title,
  children,
}: {
  security: boolean;
  title: string;
  children: React.ReactNode;
}) {
  const Icon = security ? AlertOctagon : Info;
  return (
    <div
      className={cn(
        "flex items-start gap-3 rounded-lg border p-4",
        security ? "border-destructive/50 bg-destructive/10" : "bg-muted/40",
      )}
    >
      <Icon
        aria-hidden
        className={cn(
          "mt-0.5 size-5 shrink-0",
          security ? "text-destructive" : "text-muted-foreground",
        )}
      />
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <p className="text-sm font-medium">{title}</p>
        <div className="text-sm text-muted-foreground">{children}</div>
      </div>
    </div>
  );
}

/**
 * Says, at the top of every dashboard page, that a newer HappyView is out.
 * A routine notice can be dismissed until the next version; a security one
 * comes back after a day.
 */
export function HappyViewUpdateNotice() {
  const status = useUpdateStatus();
  // Read once, lazily. The notice renders nothing until the status has
  // loaded, which is after hydration, so the server-rendered markup and
  // the first client render agree.
  const [dismissal, setDismissal] = useState<Dismissal | null>(readDismissal);

  const offer = status?.available;
  if (!status || !offer || isDismissed(offer, dismissal)) return null;

  const security = offer.severity === "security";
  const title = security
    ? `HappyView ${offer.version} is a security release. You're on ${status.current}.`
    : `HappyView ${offer.version} is available. You're on ${status.current}.`;

  const dismiss = () => {
    const d = { version: offer.version, at: Date.now() };
    writeDismissal(d);
    setDismissal(d);
  };

  return (
    <div className="px-4 pt-4 md:px-6" data-testid="happyview-update-notice">
      <Notice security={security} title={title}>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="flex flex-col gap-1">
            {status.managed ? (
              <span>
                Your hosting provider handles upgrades for this instance.
              </span>
            ) : null}
            <OfferLinks offer={offer} managed={status.managed} />
          </span>
          <Button variant="ghost" size="sm" onClick={dismiss}>
            Dismiss
          </Button>
        </div>
      </Notice>
    </div>
  );
}
