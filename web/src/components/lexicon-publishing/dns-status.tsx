"use client";

import { useState } from "react";
import {
  Check,
  CircleCheck,
  CircleHelp,
  Copy,
  TriangleAlert,
} from "lucide-react";
import { toast } from "sonner";

import { Button } from "@/components/ui/button";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import type { LexiconDnsEntry } from "@/types/lexicon-publishing";

export type DnsState =
  | { kind: "loading" }
  | { kind: "unknown" }
  | { kind: "error"; message: string }
  | { kind: "ok" }
  | { kind: "missing"; entry: LexiconDnsEntry; expectedDid: string }
  | { kind: "elsewhere"; entry: LexiconDnsEntry; expectedDid: string };

/**
 * `expectedDid` is the account the lexicon is, or is about to be, published
 * to. Without one there is no right answer to check DNS against.
 */
export function dnsState(
  entry: LexiconDnsEntry | undefined,
  loading: boolean,
  expectedDid: string | null,
): DnsState {
  if (loading) return { kind: "loading" };
  if (!entry || !expectedDid) return { kind: "unknown" };
  if (entry.error) return { kind: "error", message: entry.error };
  if (entry.did === expectedDid) return { kind: "ok" };
  if (entry.did === null) return { kind: "missing", entry, expectedDid };
  return { kind: "elsewhere", entry, expectedDid };
}

export function DnsStatus({
  state,
  coverCount,
}: {
  state: DnsState;
  /** How many publishable lexicons share this authority's record. */
  coverCount: number;
}) {
  switch (state.kind) {
    case "loading":
      return (
        <Skeleton className="h-4 w-24">
          <span className="sr-only">Checking DNS</span>
        </Skeleton>
      );
    case "unknown":
      return <span className="text-muted-foreground text-sm">—</span>;
    case "error":
      return (
        <Tooltip>
          <TooltipTrigger asChild>
            <button
              type="button"
              className="focus-visible:ring-ring/50 inline-flex items-center gap-1.5 rounded-sm text-sm whitespace-nowrap outline-none focus-visible:ring-[3px]"
              onClick={(e) => e.stopPropagation()}
            >
              <CircleHelp
                className="text-muted-foreground size-4 shrink-0"
                aria-hidden
              />
              Couldn&apos;t check
            </button>
          </TooltipTrigger>
          <TooltipContent className="max-w-xs">{state.message}</TooltipContent>
        </Tooltip>
      );
    case "ok":
      return (
        <span className="inline-flex items-center gap-1.5 text-sm whitespace-nowrap">
          <CircleCheck className="size-4 shrink-0" aria-hidden />
          Resolves here
        </span>
      );
    case "missing":
    case "elsewhere":
      return (
        <Popover>
          <PopoverTrigger asChild>
            <button
              type="button"
              className="focus-visible:ring-ring/50 inline-flex items-center gap-1.5 rounded-sm text-sm whitespace-nowrap underline decoration-dotted underline-offset-4 outline-none hover:decoration-solid focus-visible:ring-[3px]"
              onClick={(e) => e.stopPropagation()}
            >
              <TriangleAlert
                className="text-warning size-4 shrink-0"
                aria-hidden
              />
              {state.kind === "missing" ? "Not set" : "Points elsewhere"}
            </button>
          </PopoverTrigger>
          <PopoverContent
            align="start"
            className="w-[min(26rem,calc(100vw-2rem))]"
            onClick={(e) => e.stopPropagation()}
          >
            <DnsRecord
              entry={state.entry}
              expectedDid={state.expectedDid}
              currentDid={state.kind === "elsewhere" ? state.entry.did : null}
              coverCount={coverCount}
            />
          </PopoverContent>
        </Popover>
      );
  }
}

function DnsRecord({
  entry,
  expectedDid,
  currentDid,
  coverCount,
}: {
  entry: LexiconDnsEntry;
  expectedDid: string;
  currentDid: string | null;
  coverCount: number;
}) {
  const value = `did=${expectedDid}`;
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-col gap-1">
        <p className="text-sm font-medium">
          {currentDid ? "Update this DNS record" : "Add this DNS record"}
        </p>
        <p className="text-muted-foreground text-xs">
          Other apps find a published lexicon through this record, so until it
          points at the account it will not resolve.
        </p>
      </div>
      <div className="grid grid-cols-[3rem_minmax(0,1fr)_auto] items-center gap-x-2 gap-y-1.5 text-sm">
        <RecordField label="Name" value={entry.record_name} />
        <RecordField label="Type" value="TXT" copy={false} />
        <RecordField label="Value" value={value} />
      </div>
      {currentDid && (
        <p className="text-muted-foreground text-xs">
          It reads <span className="font-mono break-all">did={currentDid}</span>{" "}
          today.
        </p>
      )}
      <p className="text-muted-foreground text-xs">
        {coverCount === 1
          ? `One record per authority: this one covers ${entry.authority}.`
          : `One record per authority: this one covers all ${coverCount} lexicons under ${entry.authority}.`}
      </p>
    </div>
  );
}

function RecordField({
  label,
  value,
  copy = true,
}: {
  label: string;
  value: string;
  copy?: boolean;
}) {
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("Couldn't copy. Select the text and copy it instead.");
    }
  }

  return (
    <>
      <span className="text-muted-foreground text-xs">{label}</span>
      <code className="bg-muted/50 rounded-sm px-2 py-1 font-mono text-xs break-all select-all">
        {value}
      </code>
      <div className="size-7">
        {copy && (
          <Button
            type="button"
            variant="ghost"
            size="icon-sm"
            className="size-7"
            onClick={handleCopy}
            aria-label={copied ? `${label} copied` : `Copy ${label.toLowerCase()}`}
          >
            {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
          </Button>
        )}
      </div>
    </>
  );
}
