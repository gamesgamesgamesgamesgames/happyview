"use client";

import { useCallback, useEffect, useState } from "react";
import { Loader2, RotateCw } from "lucide-react";

import { checkLexiconDns } from "@/lib/api";
import { Button } from "@/components/ui/button";
import type { LexiconDnsCheck as Check } from "@/types/lexicon-publishing";

import { DnsRecord, RecordField } from "./dns-status";

export type LexiconDns =
  | { kind: "checking" }
  | { kind: "failed"; message: string }
  /** Names the account this lexicon publishes to. */
  | { kind: "ok"; check: Check }
  /** Names an account, and nothing yet decides which one it should name. */
  | { kind: "foreign"; check: Check }
  | { kind: "missing"; check: Check }
  | { kind: "elsewhere"; check: Check };

function classify(check: Check): LexiconDns {
  if (check.error) return { kind: "failed", message: check.error };
  if (!check.did) return { kind: "missing", check };
  if (!check.expected_did) return { kind: "foreign", check };
  if (check.did === check.expected_did) return { kind: "ok", check };
  return { kind: "elsewhere", check };
}

/**
 * Whether `_lexicon.<authority>` names the account this lexicon publishes to,
 * asked of HappyView rather than a public resolver. `expected` is the account
 * the panel believes it should name, so a publish or a re-pointed rule
 * that changes it asks again.
 */
export function useLexiconDns(nsid: string, expected: string | null) {
  const [state, setState] = useState<LexiconDns>({ kind: "checking" });
  const [rechecking, setRechecking] = useState(false);

  const run = useCallback(async () => {
    try {
      setState(classify(await checkLexiconDns(nsid)));
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
  }, [run, expected]);

  const recheck = useCallback(async () => {
    setRechecking(true);
    try {
      await run();
    } finally {
      setRechecking(false);
    }
  }, [run]);

  return { state, recheck, rechecking };
}

/** The short state a DNS row leads with, left out where its icon says it all. */
export function dnsWord(state: LexiconDns): string | undefined {
  switch (state.kind) {
    case "checking":
      return "Checking…";
    case "failed":
      return "Couldn't check";
    case "ok":
    case "foreign":
      return undefined;
    case "missing":
      return "Not set";
    case "elsewhere":
      return "Points elsewhere";
  }
}

export function CheckAgain({
  onClick,
  busy,
}: {
  onClick: () => void;
  busy: boolean;
}) {
  return (
    <Button
      type="button"
      variant="outline"
      size="xs"
      className="self-start"
      onClick={onClick}
      disabled={busy}
    >
      {busy ? (
        <Loader2
          className="animate-spin motion-reduce:animate-none"
          aria-hidden
        />
      ) : (
        <RotateCw aria-hidden />
      )}
      Check again
    </Button>
  );
}

/**
 * What sits under the DNS row's state: the record to add whenever HappyView
 * knows what it should say, and only its name when it doesn't.
 */
export function DnsDetail({
  state,
  recordName,
  coverCount,
  accountName,
}: {
  state: LexiconDns;
  recordName: string;
  /** How many publishable lexicons share this authority's record. */
  coverCount: number;
  /** The name HappyView knows a DID by, when it is one of its own destinations. */
  accountName?: (did: string) => string | null;
}) {
  const name = (
    <p className="text-muted-foreground font-mono text-xs break-all">
      {recordName}
    </p>
  );

  switch (state.kind) {
    case "checking":
    case "ok":
      return name;
    case "failed":
      return (
        <>
          {name}
          <p className="text-muted-foreground text-xs break-words">
            {state.message}
          </p>
        </>
      );
    case "foreign": {
      const did = state.check.did ?? "";
      const known = accountName?.(did);
      return (
        <>
          {name}
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 text-sm">
            <dt className="text-muted-foreground text-xs leading-5">
              Points to
            </dt>
            <dd className="min-w-0">
              <Account did={did} name={known} />
            </dd>
          </dl>
          {!known && (
            <p className="text-muted-foreground text-xs text-pretty">
              HappyView doesn&apos;t publish to this account.
            </p>
          )}
        </>
      );
    }
    case "missing":
    case "elsewhere": {
      const { check } = state;
      if (check.expected_did) {
        return (
          <DnsRecord
            entry={check}
            expectedDid={check.expected_did}
            currentDid={state.kind === "elsewhere" ? check.did : null}
            coverCount={coverCount}
            heading={false}
          />
        );
      }
      return (
        <div className="flex flex-col gap-2">
          <div className="grid grid-cols-[2.75rem_minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1.5 text-sm">
            <RecordField label="Name" value={check.record_name} />
            <RecordField label="Type" value="TXT" copy={false} />
          </div>
          <p className="text-muted-foreground text-xs">
            The value is <span className="font-mono">did=</span> followed by the
            DID of the account this lexicon is published to. Add a publishing
            rule to see it.
          </p>
        </div>
      );
    }
  }
}

/** A DID under the name HappyView knows it by, or on its own when it has none. */
function Account({ did, name }: { did: string; name?: string | null }) {
  if (!name) {
    return <span className="font-mono text-xs leading-5 break-all">{did}</span>;
  }
  return (
    <span className="flex min-w-0 flex-col">
      <span className="leading-5">{name}</span>
      <span className="text-muted-foreground font-mono text-xs break-all">
        {did}
      </span>
    </span>
  );
}
