"use client";

import { useCallback, useEffect, useState } from "react";

import { convertRetiredSpace, getRetiredSpaces } from "@/lib/api";
import { toastError } from "@/lib/format";
import { useCurrentUser } from "@/hooks/use-current-user";
import type {
  RetiredSpace,
  RetiredSpacesResponse,
  SpaceRule,
} from "@/types/spaces";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { Button } from "@/components/ui/button";

function ruleLabel(rule: SpaceRule): string {
  switch (rule.$type) {
    case "com.atproto.simplespace.defs#publicPolicy":
      return "anyone";
    case "com.atproto.simplespace.defs#memberListPolicy":
      return "members";
    case "com.atproto.simplespace.defs#managingAppPolicy":
      return `managing app ${rule.managingApp}`;
  }
}

const CONVERSIONS = {
  memberspace: {
    label: "Convert to memberspace",
    title: "Convert to a memberspace?",
    description:
      "The member list will govern reads and writes, and the managing app won't any more. Members and invites stay.",
  },
  managedspace: {
    label: "Convert to managedspace",
    title: "Convert to a managedspace?",
    description:
      "The managing app will govern reads and writes. Every member and outstanding invite is removed, and converting back won't restore them.",
  },
} as const;

function ConvertActions({
  space,
  onConverted,
}: {
  space: RetiredSpace;
  onConverted: () => void;
}) {
  const [busy, setBusy] = useState(false);

  async function convert(to: keyof typeof CONVERSIONS) {
    setBusy(true);
    try {
      await convertRetiredSpace(space.id, to);
      onConverted();
    } catch (e: unknown) {
      toastError("Couldn't convert the space", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="flex flex-wrap gap-2 pt-1">
      {(Object.keys(CONVERSIONS) as (keyof typeof CONVERSIONS)[]).map((to) => (
        <AlertDialog key={to}>
          <AlertDialogTrigger asChild>
            <Button variant="outline" size="sm" disabled={busy}>
              {CONVERSIONS[to].label}
            </Button>
          </AlertDialogTrigger>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>{CONVERSIONS[to].title}</AlertDialogTitle>
              <AlertDialogDescription>
                {CONVERSIONS[to].description}
              </AlertDialogDescription>
            </AlertDialogHeader>
            <AlertDialogFooter>
              <AlertDialogCancel>Cancel</AlertDialogCancel>
              <AlertDialogAction onClick={() => convert(to)}>
                {CONVERSIONS[to].label}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      ))}
    </div>
  );
}

/**
 * Spaces whose rules predate policies and match none. Where a managing app's
 * rule applies, their members no longer have direct access, so each one needs
 * converting. Renders nothing
 * when there are none. Which spaces they are needs the space inspector on.
 */
export function RetiredPolicyNotice() {
  const [retired, setRetired] = useState<RetiredSpacesResponse | null>(null);
  const { hasPermission } = useCurrentUser();
  const canConvert = hasPermission("spaces:update");

  const load = useCallback(() => {
    getRetiredSpaces()
      .then(setRetired)
      .catch(() => setRetired(null));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  if (!retired || retired.count === 0) return null;

  return (
    <div className="flex flex-col gap-2 rounded-lg border border-amber-500/30 bg-amber-500/5 p-3 text-sm">
      <span className="font-medium">
        {retired.count === 1
          ? "1 space uses a retired policy shape"
          : `${retired.count} spaces use a retired policy shape`}
      </span>
      <span className="text-muted-foreground">
        Each one pairs a managing app with a member list or an app allow list,
        which no space policy describes. Where the managing app&apos;s rule
        applies, to reads or to writes, its members no longer have direct
        access; it goes through the managing app&apos;s decision instead. A
        managing app that doesn&apos;t answer this instance&apos;s access checks
        leaves those members with no access until the space is converted.
        Convert each one to a memberspace or a managedspace.
      </span>
      {retired.spaces ? (
        <ul className="flex flex-col gap-1">
          {retired.spaces.map((space) => (
            <li key={space.id} className="flex flex-col gap-0.5">
              <span className="font-mono text-xs break-all">{space.uri}</span>
              <span className="text-muted-foreground text-xs">
                Reads: {ruleLabel(space.readPolicy)}. Writes:{" "}
                {ruleLabel(space.writePolicy)}.
                {space.appAccess.allowed
                  ? ` Apps: ${space.appAccess.allowed.length} allowed.`
                  : ""}
              </span>
              {canConvert && (
                <ConvertActions space={space} onConverted={load} />
              )}
            </li>
          ))}
        </ul>
      ) : (
        <span className="text-muted-foreground text-xs">
          The server log names each one at startup. Turn the space inspector on
          to list them here.
        </span>
      )}
    </div>
  );
}
