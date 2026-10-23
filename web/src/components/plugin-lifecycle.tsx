"use client";

import { AlertOctagon, AlertTriangle, Info } from "lucide-react";

import { cn } from "@/lib/utils";
import type {
  ChannelEntry,
  LifecycleState,
  OfficialPluginSummary,
} from "@/types/plugins";
import { Badge } from "@/components/ui/badge";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export const STATE_LABEL: Record<LifecycleState, string> = {
  active: "Active",
  deprecated: "Deprecated",
  unavailable: "Unavailable",
  retired: "Retired",
};

/** The registry's tracks, in the order an operator weighs them. */
const CHANNEL_ORDER = ["latest", "beta", "alpha"];

export function orderedChannels(names: Iterable<string>): string[] {
  const rank = (name: string) => {
    const i = CHANNEL_ORDER.indexOf(name);
    return i === -1 ? CHANNEL_ORDER.length : i;
  };
  return [...names].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
}

/**
 * A catalogue entry's channels, but only when the entry is the project the
 * plugin was installed from. Another publisher's project can share the name.
 */
export function channelsFor(
  entry: OfficialPluginSummary | undefined,
  did: string | null,
): Record<string, ChannelEntry> | null {
  if (!entry || !did || entry.publisher_did !== did) return null;
  return entry.channels;
}

/**
 * A version's state, where it says something. An active version carries no
 * badge, so only the states an operator has to act on draw the eye.
 */
export function ReleaseStateBadge({
  state,
  installable = true,
  className,
}: {
  state: LifecycleState | null | undefined;
  installable?: boolean;
  className?: string;
}) {
  if (state === "retired") {
    return (
      <Badge variant="destructive" className={className}>
        {STATE_LABEL.retired}
      </Badge>
    );
  }
  if (state === "deprecated" || state === "unavailable") {
    return (
      <Badge
        variant="outline"
        className={cn(
          state === "deprecated" &&
            "border-amber-500/50 text-amber-700 dark:text-amber-400",
          className,
        )}
      >
        {STATE_LABEL[state]}
      </Badge>
    );
  }
  if (!installable) {
    return (
      <Badge variant="outline" className={className}>
        Not installable
      </Badge>
    );
  }
  return null;
}

/** What a channel's version is called when it can't be installed. */
export function unavailableChannelCopy(
  channel: string,
  state: LifecycleState | null | undefined,
): string {
  return state === "retired"
    ? `The ${channel} channel points to a version that's retired`
    : `The ${channel} channel points to a version that isn't available`;
}

const TONE = {
  danger: {
    box: "border-destructive/50 bg-destructive/10",
    icon: AlertOctagon,
    iconClass: "text-destructive",
  },
  warning: {
    box: "border-amber-500/50 bg-amber-500/10",
    icon: AlertTriangle,
    iconClass: "text-amber-500",
  },
  info: {
    box: "bg-muted/40",
    icon: Info,
    iconClass: "text-muted-foreground",
  },
} as const;

export function LifecycleNotice({
  tone,
  title,
  children,
}: {
  tone: keyof typeof TONE;
  title: string;
  children?: React.ReactNode;
}) {
  const { box, icon: Icon, iconClass } = TONE[tone];
  return (
    <div className={cn("flex items-start gap-3 rounded-lg border p-4", box)}>
      <Icon aria-hidden className={cn("mt-0.5 size-5 shrink-0", iconClass)} />
      <div className="flex min-w-0 flex-col gap-1">
        <p className="text-sm font-medium">{title}</p>
        {children ? (
          <div className="text-sm text-muted-foreground">{children}</div>
        ) : null}
      </div>
    </div>
  );
}

/**
 * Picks a channel. Each option names the version it points at and, when it
 * isn't active, that version's state, so the choice is made knowing what it
 * installs.
 */
export function ChannelSelect({
  id,
  channels,
  names,
  value,
  onValueChange,
  disabled,
}: {
  id?: string;
  /** What each channel points at, when the catalogue holds the project. */
  channels: Record<string, ChannelEntry> | null;
  /** The channels to offer when `channels` is null. */
  names?: string[];
  value: string;
  onValueChange: (channel: string) => void;
  disabled?: boolean;
}) {
  const options = orderedChannels(
    channels ? Object.keys(channels) : (names ?? CHANNEL_ORDER),
  );
  return (
    <Select value={value} onValueChange={onValueChange} disabled={disabled}>
      <SelectTrigger id={id} className="w-56">
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((name) => {
          const head = channels?.[name];
          const state = head
            ? head.state !== "active"
              ? STATE_LABEL[head.state].toLowerCase()
              : head.installable
                ? null
                : "not installable"
            : null;
          return (
            <SelectItem key={name} value={name}>
              {name}
              {head ? (
                <span className="text-xs text-muted-foreground">
                  <span className="font-mono tabular-nums">{head.version}</span>
                  {state ? `, ${state}` : null}
                </span>
              ) : null}
            </SelectItem>
          );
        })}
      </SelectContent>
    </Select>
  );
}
