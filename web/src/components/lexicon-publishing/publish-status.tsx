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
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type {
  PublishableLexicon,
  PublishStatus,
} from "@/types/lexicon-publishing";

import { lastFailure } from "./model";

type StatusLook = {
  label: string;
  short: string;
  icon: LucideIcon;
  tone: string;
  explain: string;
};

// The icon carries the colour and the word carries the meaning, so the text
// stays at foreground contrast and the status reads without colour at all.
const LOOKS: Record<PublishStatus, StatusLook> = {
  current: {
    label: "Published",
    short: "Published",
    icon: CircleCheck,
    tone: "text-emerald-700 dark:text-emerald-400",
    explain: "The published record matches this lexicon.",
  },
  outdated: {
    label: "Outdated",
    short: "Outdated",
    icon: CircleDot,
    tone: "text-amber-700 dark:text-amber-400",
    explain: "The lexicon changed after it was published.",
  },
  moved: {
    label: "Published elsewhere",
    short: "Elsewhere",
    icon: MoveRight,
    tone: "text-amber-700 dark:text-amber-400",
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
    tone: "text-amber-700 dark:text-amber-400",
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
  className,
}: {
  lexicon: PublishableLexicon;
  publishing?: boolean;
  compact?: boolean;
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
