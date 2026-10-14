"use client";

import {
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleDot,
  CircleSlash,
  Loader2,
  Minus,
  MoveRight,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";

import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type {
  PublishableLexicon,
  PublishStatus,
} from "@/types/lexicon-publishing";

import { asSentence, lastFailure } from "./model";

type StatusLook = {
  label: string;
  short: string;
  icon: LucideIcon;
  tone: string;
  explain: string;
};

// The word carries the meaning and the icon's shape backs it up, so the status
// reads without colour at all. Only the states asking for attention tint their
// icon, and the text stays at foreground contrast.
const LOOKS: Record<PublishStatus, StatusLook> = {
  current: {
    label: "Published",
    short: "Published",
    icon: CircleCheck,
    tone: "",
    explain: "The published record matches this lexicon.",
  },
  outdated: {
    label: "Outdated",
    short: "Outdated",
    icon: CircleDot,
    tone: "text-warning",
    explain: "The lexicon changed after it was published.",
  },
  moved: {
    label: "Published elsewhere",
    short: "Elsewhere",
    icon: MoveRight,
    tone: "text-warning",
    explain:
      "Published to a different account than its default names. Publishing writes it to the new one.",
  },
  unpublished: {
    label: "Not published",
    short: "Not published",
    icon: CircleDashed,
    tone: "text-muted-foreground",
    explain: "A default covers this lexicon, but it has not been published.",
  },
  withdrawn: {
    label: "Withdrawn",
    short: "Withdrawn",
    icon: CircleSlash,
    tone: "text-muted-foreground",
    explain:
      "Unpublished by hand. Auto-publish leaves it alone until you publish it again.",
  },
  target_unavailable: {
    label: "Destination unavailable",
    short: "Unavailable",
    icon: TriangleAlert,
    tone: "text-warning",
    explain: "The account its default names cannot receive records right now.",
  },
  no_rule: {
    label: "No default",
    short: "No default",
    icon: Minus,
    tone: "text-muted-foreground",
    explain: "No publishing default covers this lexicon.",
  },
};

export function statusLabel(status: PublishStatus): string {
  return LOOKS[status].label;
}

export function statusExplanation(status: PublishStatus): string {
  return LOOKS[status].explain;
}

export function PublishStatusBadge({
  lexicon,
  publishing = false,
  compact = false,
  explain = false,
  className,
}: {
  lexicon: PublishableLexicon;
  publishing?: boolean;
  compact?: boolean;
  /**
   * Opens the explanation, the destination's reason and any failure on press,
   * for places that show the badge without them beside it.
   */
  explain?: boolean;
  className?: string;
}) {
  if (publishing) {
    return (
      <span className={cn("inline-flex items-center gap-1.5 text-sm whitespace-nowrap", className)}>
        <Loader2
          className="text-muted-foreground size-4 animate-spin motion-reduce:animate-none"
          aria-hidden
        />
        Publishing…
      </span>
    );
  }

  const look = LOOKS[lexicon.status];
  const Icon = look.icon;
  const failure = lastFailure(lexicon);
  const reason =
    lexicon.status === "target_unavailable"
      ? lexicon.target?.unavailable_reason
      : null;

  if (explain) {
    return (
      <Popover>
        <PopoverTrigger asChild>
          <button
            type="button"
            className={cn(
              "focus-visible:ring-ring/50 inline-flex items-center gap-1.5 rounded-sm text-sm whitespace-nowrap underline decoration-dotted underline-offset-4 outline-none hover:decoration-solid focus-visible:ring-[3px]",
              className,
            )}
            onClick={(e) => e.stopPropagation()}
          >
            <Icon className={cn("size-4 shrink-0", look.tone)} aria-hidden />
            {compact ? look.short : look.label}
            {failure && (
              <>
                <CircleAlert className="text-destructive size-4" aria-hidden />
                <span className="sr-only">, last publish failed</span>
              </>
            )}
          </button>
        </PopoverTrigger>
        <PopoverContent
          align="start"
          className="flex w-[min(20rem,calc(100vw-2rem))] flex-col gap-2 text-sm"
          onClick={(e) => e.stopPropagation()}
        >
          <p className="font-medium">{look.label}</p>
          <p className="text-muted-foreground">
            {reason ? asSentence(reason) : look.explain}
          </p>
          {failure && (
            <p className="text-destructive">Last publish failed: {failure}</p>
          )}
        </PopoverContent>
      </Popover>
    );
  }

  return (
    <span className={cn("inline-flex items-center gap-1.5 text-sm whitespace-nowrap", className)}>
      <Icon className={cn("size-4 shrink-0", look.tone)} aria-hidden />
      <span title={reason ?? look.explain}>
        {compact ? look.short : look.label}
      </span>
      {failure && <FailureMark message={failure} />}
    </span>
  );
}

function FailureMark({ message }: { message: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          className="text-destructive focus-visible:ring-ring/50 inline-flex rounded-sm outline-none focus-visible:ring-[3px]"
          aria-label={`Last publish failed: ${message}`}
          onClick={(e) => e.stopPropagation()}
        >
          <CircleAlert className="size-4" aria-hidden />
        </button>
      </TooltipTrigger>
      <TooltipContent className="max-w-xs">
        Last publish failed: {message}
      </TooltipContent>
    </Tooltip>
  );
}
