import Link from "next/link";
import { Info } from "lucide-react";

import { cn } from "@/lib/utils";

/** What to set up before anything can publish, with the two ways to do it. */
export function NoTargets({ className }: { className?: string }) {
  return (
    <div
      className={cn(
        "bg-muted/40 flex items-start gap-3 rounded-lg border p-4 text-sm",
        className,
      )}
    >
      <Info
        className="text-muted-foreground mt-0.5 size-4 shrink-0"
        aria-hidden
      />
      <p>
        No account is set up for publishing. Attach one in{" "}
        <Link
          href="/dashboard/settings/service-identity"
          className="font-medium underline underline-offset-4"
        >
          Service Identity
        </Link>{" "}
        or link a repo in{" "}
        <Link
          href="/dashboard/settings/linked-repos"
          className="font-medium underline underline-offset-4"
        >
          Linked Repos
        </Link>
        .
      </p>
    </div>
  );
}
