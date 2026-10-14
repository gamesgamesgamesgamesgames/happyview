"use client";

import { useCallback, useEffect, useState } from "react";
import {
  CircleCheck,
  CircleHelp,
  Info,
  Loader2,
  RotateCw,
  TriangleAlert,
} from "lucide-react";

import { checkLexiconDns } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import type { LexiconDnsCheck as Check } from "@/types/lexicon-publishing";

import { DnsRecord, RecordField } from "./dns-status";

type State =
  | { kind: "checking" }
  | { kind: "failed"; message: string }
  | { kind: "done"; check: Check };

/**
 * Whether `_lexicon.<authority>` names the account this lexicon publishes to,
 * asked of HappyView rather than a public resolver, with the record to add
 * whenever HappyView knows what it should say.
 */
export function LexiconDnsCheck({
  nsid,
  coverCount,
}: {
  nsid: string;
  /** How many publishable lexicons share this authority's record. */
  coverCount: number;
}) {
  const [state, setState] = useState<State>({ kind: "checking" });
  const [rechecking, setRechecking] = useState(false);

  const run = useCallback(async () => {
    try {
      setState({ kind: "done", check: await checkLexiconDns(nsid) });
    } catch (e) {
      setState({
        kind: "failed",
        message: e instanceof Error ? e.message : String(e),
      });
    }
  }, [nsid]);

  useEffect(() => {
    setState({ kind: "checking" });
    run();
  }, [run]);

  async function recheck() {
    setRechecking(true);
    try {
      await run();
    } finally {
      setRechecking(false);
    }
  }

  if (state.kind === "checking") {
    return (
      <Skeleton className="h-4 w-28">
        <span className="sr-only">Checking DNS</span>
      </Skeleton>
    );
  }

  const again = (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className="self-start"
      onClick={recheck}
      disabled={rechecking}
    >
      {rechecking ? (
        <Loader2 className="animate-spin motion-reduce:animate-none" aria-hidden />
      ) : (
        <RotateCw aria-hidden />
      )}
      Check again
    </Button>
  );

  if (state.kind === "failed" || state.check.error) {
    const message =
      state.kind === "failed" ? state.message : state.check.error;
    return (
      <div className="flex flex-col gap-2">
        <Line icon={<CircleHelp className="text-muted-foreground" />}>
          Couldn&apos;t check
        </Line>
        <p className="text-muted-foreground text-xs break-words">{message}</p>
        {again}
      </div>
    );
  }

  const { check } = state;

  if (check.did && check.did === check.expected_did) {
    return (
      <Line icon={<CircleCheck />}>Set, and points at the right account</Line>
    );
  }

  if (check.did && !check.expected_did) {
    return (
      <div className="flex flex-col gap-2">
        <Line icon={<Info className="text-muted-foreground" />}>
          Set to another account
        </Line>
        <p className="text-muted-foreground text-xs">
          It names <span className="font-mono break-all">{check.did}</span>.
          Once a default sends this lexicon somewhere, this says whether that
          matches.
        </p>
        {again}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <Line icon={<TriangleAlert className="text-warning" />}>
        {check.did ? "Points at a different account" : "Not set yet"}
      </Line>
      {check.expected_did ? (
        <DnsRecord
          entry={check}
          expectedDid={check.expected_did}
          currentDid={check.did}
          coverCount={coverCount}
        />
      ) : (
        <div className="flex flex-col gap-2">
          <p className="text-muted-foreground text-xs">
            Other apps find this lexicon through a TXT record at this name. Its
            value is <span className="font-mono">did=</span> and the account it
            publishes to, which appears here once a default sends it somewhere.
          </p>
          <div className="grid grid-cols-[3rem_minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1.5 text-sm">
            <RecordField label="Name" value={check.record_name} />
            <RecordField label="Type" value="TXT" copy={false} />
          </div>
        </div>
      )}
      <p className="text-muted-foreground text-xs">
        DNS changes can take a while to reach HappyView.
      </p>
      {again}
    </div>
  );
}

function Line({
  icon,
  children,
}: {
  icon: React.ReactElement<{ className?: string }>;
  children: React.ReactNode;
}) {
  return (
    <p className="inline-flex items-center gap-1.5 text-sm [&_svg]:size-4 [&_svg]:shrink-0">
      <span aria-hidden className="inline-flex">
        {icon}
      </span>
      {children}
    </p>
  );
}
