"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import {
  ArrowUpRight,
  CircleAlert,
  CircleCheck,
  CircleDashed,
  CircleDot,
  CircleHelp,
  CircleSlash,
  Info,
  Loader2,
  Minus,
  TriangleAlert,
  type LucideIcon,
} from "lucide-react";

import { getLexiconPublishing } from "@/lib/api";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";
import { relativeTime } from "@/components/linked-repos/linked-repo-detail";
import type {
  PublishableLexicon,
  PublishTarget,
} from "@/types/lexicon-publishing";

import {
  CheckAgain,
  DnsDetail,
  dnsWord,
  useLexiconDns,
  type LexiconDns,
} from "./lexicon-dns-check";
import {
  expectedDid,
  lastFailure,
  nsidAuthority,
  targetName,
  unavailableSentence,
} from "./model";
import { NoTargets } from "./no-targets";
import { TargetName } from "./target-name";
import {
  canPublishNow,
  isPublished,
  publishLabel,
  UnpublishButton,
} from "./row-actions";
import { usePublish } from "./use-publish";

const PUBLISHING_HREF = "/dashboard/settings/lexicon-publishing";

/**
 * Whether other apps can use this lexicon, and the two things that takes: a
 * schema record in the account its rule names, and a DNS record naming
 * that account. Best-effort like the scripts panel: a failed load leaves the
 * editor usable.
 */
export function PublishingPanel({
  nsid,
  revision,
  canPublish,
}: {
  nsid: string;
  /** A saved edit changes the record's CID, so the status is re-read. */
  revision: number;
  canPublish: boolean;
}) {
  const [lexicon, setLexicon] = useState<PublishableLexicon | null>(null);
  const [usable, setUsable] = useState(true);
  const [targets, setTargets] = useState<PublishTarget[]>([]);
  const [coverCount, setCoverCount] = useState(1);
  const [state, setState] = useState<"loading" | "ready" | "failed">("loading");

  const authority = nsidAuthority(nsid);

  const read = useCallback(
    () =>
      getLexiconPublishing().then(
        (overview) => {
          setLexicon(overview.lexicons.find((l) => l.nsid === nsid) ?? null);
          setUsable(overview.usable);
          setTargets(overview.targets);
          setCoverCount(
            overview.lexicons.filter((l) => nsidAuthority(l.nsid) === authority)
              .length || 1,
          );
          setState("ready");
        },
        () => setState("failed"),
      ),
    [nsid, authority],
  );

  useEffect(() => {
    read();
  }, [read, revision]);

  const { busy, publish, unpublish, announcement } = usePublish(read);
  const working = busy.has(nsid);
  const dns = useLexiconDns(nsid, lexicon ? expectedDid(lexicon) : null);
  const accountName = (did: string) => {
    const t = targets.find((t) => t.did === did);
    return t ? targetName(t) : null;
  };
  // Pointing at one of this instance's own destinations reads as right even
  // before a rule picks one; any other account HappyView can't vouch for.
  const dnsKnown =
    dns.state.kind === "foreign" &&
    Boolean(accountName(dns.state.check.did ?? ""));

  return (
    <section aria-labelledby="publishing-panel-title">
      <div className="mb-3 flex items-center justify-between gap-2">
        <Label id="publishing-panel-title" className="text-muted-foreground">
          Publishing
        </Label>
        {state === "ready" && (
          <Link
            href={PUBLISHING_HREF}
            className="text-muted-foreground hover:text-foreground focus-visible:ring-ring/50 inline-flex items-center gap-0.5 rounded-sm text-xs outline-none focus-visible:ring-[3px]"
          >
            {usable ? "All rules" : "Set up publishing"}
            <ArrowUpRight className="size-3.5" aria-hidden />
          </Link>
        )}
      </div>

      {state === "loading" ? (
        <div className="flex flex-col gap-2 rounded-md border p-4">
          <Skeleton className="h-5 w-40" />
          <Skeleton className="h-3 w-52" />
          <Skeleton className="mt-3 h-4 w-full" />
          <Skeleton className="h-4 w-full" />
        </div>
      ) : state === "failed" || !lexicon ? (
        <p className="text-muted-foreground rounded-md border border-dashed p-4 text-sm">
          Couldn&apos;t load publishing for this lexicon.
        </p>
      ) : (
        <div className="divide-y rounded-md border">
          {usable ? (
            <Answer
              lexicon={lexicon}
              dns={dns.state}
              canPublish={canPublish}
              working={working}
              onPublish={() => publish([nsid])}
            />
          ) : (
            <NoTargets className="rounded-none border-0 bg-transparent p-4" />
          )}

          <ol
            className="flex flex-col gap-4 p-4"
            aria-label="Publishing checklist"
          >
            {usable && <RecordStep lexicon={lexicon} working={working} />}
            <Step
              look={dnsLook(dns.state, dnsKnown)}
              title="DNS record"
              word={dnsWord(dns.state)}
            >
              <DnsDetail
                state={dns.state}
                recordName={`_lexicon.${authority}`}
                coverCount={coverCount}
                accountName={accountName}
              />
              {dns.state.kind !== "checking" &&
                dns.state.kind !== "ok" &&
                !dnsKnown && (
                  <>
                    {(dns.state.kind === "missing" ||
                      dns.state.kind === "elsewhere") && (
                      <p className="text-muted-foreground text-xs">
                        DNS changes can take a while to propagate.
                      </p>
                    )}
                    <CheckAgain onClick={dns.recheck} busy={dns.rechecking} />
                  </>
                )}
            </Step>
          </ol>

          {usable && (
            <Details lexicon={lexicon} canPublish={canPublish}>
              {canPublish && isPublished(lexicon) && (
                <UnpublishButton
                  nsid={lexicon.nsid}
                  did={lexicon.publication?.did}
                  autoPublish={lexicon.auto_publish}
                  busy={working}
                  onConfirm={() => unpublish(nsid)}
                />
              )}
            </Details>
          )}
        </div>
      )}

      <p className="sr-only" aria-live="polite" role="status">
        {announcement}
      </p>
    </section>
  );
}

type Look = { icon: LucideIcon; tone: string; spin?: boolean };

const DONE: Look = { icon: CircleCheck, tone: "text-success" };
const ATTENTION: Look = { icon: TriangleAlert, tone: "text-warning" };
const CHECKING: Look = {
  icon: Loader2,
  tone: "text-muted-foreground",
  spin: true,
};

function Glyph({ look, className }: { look: Look; className?: string }) {
  const Icon = look.icon;
  return (
    <Icon
      aria-hidden
      className={cn(
        "size-4 shrink-0",
        look.tone,
        look.spin && "animate-spin motion-reduce:animate-none",
        className,
      )}
    />
  );
}

type Verdict = {
  look: Look;
  title: string;
  /** What is true now. */
  fact?: React.ReactNode;
  /** What to do about it, shown only to someone who can. */
  next?: React.ReactNode;
};

function verdict(
  lexicon: PublishableLexicon,
  dns: LexiconDns,
  canPublish: boolean,
): Verdict {
  const where = lexicon.target ? targetName(lexicon.target) : null;
  switch (lexicon.status) {
    case "no_rule":
      return {
        look: { icon: Minus, tone: "text-muted-foreground" },
        title: "No publishing rule",
        fact: (
          <>
            No rule covers this lexicon.{" "}
            <Link
              href={PUBLISHING_HREF}
              className="text-foreground underline underline-offset-4"
            >
              {canPublish ? "Add a rule" : "See rules"}
            </Link>
          </>
        ),
      };
    case "target_unavailable":
      return {
        look: ATTENTION,
        title: "Destination unavailable",
        fact: unavailableSentence(lexicon.target),
      };
    case "withdrawn":
      return {
        look: { icon: CircleSlash, tone: "text-muted-foreground" },
        title: "Unpublished",
        fact: lexicon.auto_publish
          ? "It was unpublished manually. Auto-publish skips it until it's published again."
          : "It was unpublished manually.",
        next: "Publish it to make it available again.",
      };
    case "unpublished":
      return {
        look: { icon: CircleDashed, tone: "text-muted-foreground" },
        title: "Not published",
        fact: where ? `It will be published to ${where}.` : undefined,
      };
    case "moved":
      return {
        look: ATTENTION,
        title: "Published to a different account",
        fact: where
          ? `Its rule now points to ${where}.`
          : "Its rule now points to another account.",
        next: "Republish to move it.",
      };
    case "outdated":
      return {
        look: { icon: CircleDot, tone: "text-warning" },
        title: "Outdated",
        fact: "The lexicon has changed since it was published.",
        next: "Republish to update it.",
      };
    case "current":
      switch (dns.kind) {
        case "ok":
        case "foreign":
          return { look: DONE, title: "Published and resolvable" };
        case "checking":
          return {
            look: DONE,
            title: "Published",
            fact: "Checking DNS…",
          };
        case "failed":
          return {
            look: DONE,
            title: "Published",
            fact: "Couldn't check the DNS record.",
          };
        case "missing":
          return {
            look: ATTENTION,
            title: "Published, but not resolvable",
            fact: "Add the DNS record below so other apps can find it.",
          };
        case "elsewhere":
          return {
            look: ATTENTION,
            title: "Published, but not resolvable",
            fact: "The DNS record points to a different account. Update it below.",
          };
      }
  }
}

function Answer({
  lexicon,
  dns,
  canPublish,
  working,
  onPublish,
}: {
  lexicon: PublishableLexicon;
  dns: LexiconDns;
  canPublish: boolean;
  working: boolean;
  onPublish: () => void;
}) {
  const v = verdict(lexicon, dns, canPublish);
  const failure = lastFailure(lexicon);
  const action = canPublish && canPublishNow(lexicon);

  return (
    <div className="flex flex-col gap-3 p-4">
      <div className="grid grid-cols-[1.25rem_minmax(0,1fr)] gap-x-2.5 gap-y-1">
        {working ? (
          <Glyph look={CHECKING} className="mt-px size-5" />
        ) : (
          <Glyph look={v.look} className="mt-px size-5" />
        )}
        <p className="text-base leading-6 font-medium text-pretty">
          {working ? "Publishing…" : v.title}
        </p>
        {(v.fact || (canPublish && v.next)) && !working && (
          <p className="text-muted-foreground col-start-2 text-sm text-pretty">
            {v.fact}
            {v.fact && canPublish && v.next ? " " : null}
            {canPublish && v.next}
          </p>
        )}
        {failure && !working && (
          <p className="text-destructive col-start-2 flex gap-1.5 text-sm break-words">
            <CircleAlert className="mt-0.5 size-4 shrink-0" aria-hidden />
            <span>The last publish failed: {failure}</span>
          </p>
        )}
      </div>
      {action && (
        <Button
          size="sm"
          className="ms-[1.875rem] self-start"
          onClick={onPublish}
          disabled={working}
        >
          {working && (
            <Loader2
              className="animate-spin motion-reduce:animate-none"
              aria-hidden
            />
          )}
          {publishLabel(lexicon)}
        </Button>
      )}
    </div>
  );
}

function Step({
  look,
  title,
  word,
  children,
}: {
  look: Look;
  title: string;
  /** Left out when the icon already says it all. */
  word?: string;
  children?: React.ReactNode;
}) {
  return (
    <li className="grid grid-cols-[1rem_minmax(0,1fr)] gap-x-2.5 gap-y-1.5">
      <Glyph look={look} className="mt-0.5" />
      <p className="flex flex-wrap items-baseline justify-between gap-x-3 text-sm">
        <span>{title}</span>
        {word && <span className="font-medium">{word}</span>}
      </p>
      {children && (
        <div className="col-start-2 flex min-w-0 flex-col gap-2">
          {children}
        </div>
      )}
    </li>
  );
}

function RecordStep({
  lexicon,
  working,
}: {
  lexicon: PublishableLexicon;
  working: boolean;
}) {
  const p = lexicon.publication;
  const look: Look = working
    ? CHECKING
    : lexicon.status === "current"
      ? DONE
      : lexicon.status === "outdated" ||
          lexicon.status === "moved" ||
          lexicon.status === "target_unavailable"
        ? ATTENTION
        : { icon: CircleDashed, tone: "text-muted-foreground" };

  return (
    <Step look={look} title="Published">
      {p?.uri && p.published_at ? (
        <p className="text-muted-foreground text-xs">
          {lexicon.status === "moved" && p.did ? (
            <>
              In <span className="font-mono break-all">{p.did}</span>,
              published{" "}
            </>
          ) : (
            "Published "
          )}
          <time
            dateTime={p.published_at}
            title={new Date(p.published_at).toLocaleString()}
          >
            {relativeTime(p.published_at)}
          </time>
        </p>
      ) : null}
    </Step>
  );
}

/** `known`: a set record names one of this instance's destinations. */
function dnsLook(state: LexiconDns, known: boolean): Look {
  switch (state.kind) {
    case "checking":
      return CHECKING;
    case "ok":
      return DONE;
    case "failed":
      return { icon: CircleHelp, tone: "text-muted-foreground" };
    case "foreign":
      return known ? DONE : { icon: Info, tone: "text-muted-foreground" };
    case "missing":
    case "elsewhere":
      return ATTENTION;
  }
}

function Details({
  lexicon,
  canPublish,
  children,
}: {
  lexicon: PublishableLexicon;
  canPublish: boolean;
  children?: React.ReactNode;
}) {
  const ruled = Boolean(lexicon.rule_pattern);
  if (!ruled && !children) return null;

  return (
    <div className="flex flex-col gap-4 p-4">
      {ruled && (
        <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-sm">
          <dt className="text-muted-foreground">Destination</dt>
          <dd className="flex min-w-0 flex-col gap-0.5">
            {lexicon.target ? (
              <TargetName target={lexicon.target} className="block" />
            ) : (
              "None"
            )}
            <span className="text-muted-foreground text-xs text-pretty">
              Set by the{" "}
              <span className="font-mono">{lexicon.rule_pattern}</span> rule.{" "}
              <Link
                href={PUBLISHING_HREF}
                className="text-foreground underline underline-offset-4"
              >
                {canPublish ? "Change" : "See rules"}
              </Link>
            </span>
          </dd>

          <dt className="text-muted-foreground">Auto-publish</dt>
          <dd>{lexicon.auto_publish ? "On" : "Off"}</dd>
        </dl>
      )}
      {children && <div className="flex justify-end">{children}</div>}
    </div>
  );
}
