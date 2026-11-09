"use client";

import { useEffect, useState } from "react";

import { getRetiredSpaces } from "@/lib/api";
import type { RetiredSpacesResponse, SpaceRule } from "@/types/spaces";

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

/**
 * Spaces whose rules predate policies and match none. Where a managing app's
 * rule applies, their members no longer have direct access, so each one needs
 * converting. Renders nothing
 * when there are none. Which spaces they are needs the space inspector on.
 */
export function RetiredPolicyNotice() {
  const [retired, setRetired] = useState<RetiredSpacesResponse | null>(null);

  useEffect(() => {
    getRetiredSpaces()
      .then(setRetired)
      .catch(() => setRetired(null));
  }, []);

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
        access; the managing app decides. Convert each one to a memberspace or a
        managedspace.
      </span>
      {retired.spaces ? (
        <ul className="flex flex-col gap-1">
          {retired.spaces.map((space) => (
            <li key={space.id} className="flex flex-col">
              <span className="font-mono text-xs break-all">{space.uri}</span>
              <span className="text-muted-foreground text-xs">
                Reads: {ruleLabel(space.readPolicy)}. Writes:{" "}
                {ruleLabel(space.writePolicy)}.
                {space.appAccess.allowed
                  ? ` Apps: ${space.appAccess.allowed.length} allowed.`
                  : ""}
              </span>
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
