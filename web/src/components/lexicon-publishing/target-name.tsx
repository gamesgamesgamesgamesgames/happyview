import { cn } from "@/lib/utils";
import type { PublishTarget } from "@/types/lexicon-publishing";

import { nameIsDid, targetName } from "./model";

/**
 * A destination's name, set as an identifier when it falls back to the DID,
 * with the full DID on hover since a DID rarely fits.
 */
export function TargetName({
  target,
  className,
}: {
  target: PublishTarget;
  className?: string;
}) {
  const isDid = nameIsDid(target);
  return (
    <span
      className={cn("truncate", isDid && "font-mono", className)}
      title={isDid ? (target.did ?? undefined) : undefined}
    >
      {targetName(target)}
    </span>
  );
}
