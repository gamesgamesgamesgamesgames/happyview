import type { ReactNode } from "react";

import { AnimatedNumber } from "./animated-number";
import {
  type RecordsCount,
  recordsCountParts,
  recordsFraction,
} from "./records-progress";

/**
 * One progress row counting records: a label, the count, a bar when the
 * total is known, and a `detail` slot beside the count for whatever else the
 * row has to say (a rate, an ETA, failures).
 */
export function RecordsProgress({
  label,
  active,
  detail,
  ...count
}: RecordsCount & {
  label: string;
  /** Running now: an unknown total pulses rather than sitting still. */
  active: boolean;
  detail?: ReactNode;
}) {
  const fraction = recordsFraction(count);
  const { done, total, estimated, noun } = recordsCountParts(count);
  return (
    <div className="flex flex-col gap-2 px-3 py-2.5 text-sm">
      <div className="flex items-center justify-between gap-2">
        <span>{label}</span>
        <span className="text-xs text-muted-foreground tabular-nums">
          <AnimatedNumber value={done} />
          {total != null && (
            <>
              {` of ${estimated ? "about " : ""}`}
              <AnimatedNumber value={total} />
            </>
          )}
          {` ${noun}`}
        </span>
      </div>
      <div
        role="progressbar"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={
          fraction == null ? undefined : Math.round(fraction * 100)
        }
        className="h-1.5 w-full overflow-hidden rounded-full bg-muted"
      >
        <div
          className={`h-full rounded-full bg-blue-500 ${
            fraction == null
              ? `w-1/3 ${active ? "animate-pulse" : ""}`
              : "transition-[width]"
          }`}
          style={fraction == null ? undefined : { width: `${fraction * 100}%` }}
        />
      </div>
      {detail && (
        <div className="flex items-center justify-between gap-2 text-xs text-muted-foreground tabular-nums">
          {detail}
        </div>
      )}
    </div>
  );
}
