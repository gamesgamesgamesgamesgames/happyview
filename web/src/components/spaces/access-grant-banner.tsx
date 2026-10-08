"use client";

import { Button } from "@/components/ui/button";
import type { AccessGrant } from "@/types/spaces";
import { AccountName } from "@/components/account-name";

function formatRemaining(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")} remaining`;
}

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
        <span className="text-muted-foreground break-words">{grant.reason}</span>
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
