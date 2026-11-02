"use client";

import { formatRemaining } from "@/lib/access-grant";
import { Button } from "@/components/ui/button";
import type { AccessGrant } from "@/types/spaces";
import { AccountName } from "@/components/account-name";

export function AccessGrantBanner({
  grant,
  remainingMs,
  onEnd,
}: {
  grant: AccessGrant;
  remainingMs: number;
  onEnd: () => void;
}) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-3 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3">
      <div className="flex min-w-0 flex-col gap-1 text-sm">
        <span className="font-medium">
          {grant.scope === "space" ? (
            "Access to this space"
          ) : (
            <>
              Access to <AccountName did={grant.target} />
            </>
          )}
        </span>
        <span className="text-muted-foreground break-words">
          {grant.reason}
        </span>
        <span className="text-muted-foreground text-xs tabular-nums">
          {formatRemaining(remainingMs)}
        </span>
      </div>
      <Button variant="outline" size="sm" onClick={onEnd}>
        End access
      </Button>
    </div>
  );
}
