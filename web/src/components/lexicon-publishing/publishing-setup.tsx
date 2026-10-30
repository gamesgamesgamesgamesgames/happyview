import Link from "next/link";
import { ArrowRight, CircleDashed, TriangleAlert } from "lucide-react";

import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import type { PublishTarget } from "@/types/lexicon-publishing";

import { asSentence, isRedacted } from "./model";
import { TargetName } from "./target-name";

const SERVICE_IDENTITY_HREF = "/dashboard/settings/service-identity";
const LINKED_REPOS_HREF = "/dashboard/settings/linked-repos";

type Fix = {
  /** Something exists but needs attention, as against nothing set up. */
  blocked: boolean;
  problem: string;
  action: string;
};

// Keyed on the reasons src/lexicon_publishing/target.rs writes. A reason this
// doesn't recognise is shown as the server wrote it.

function instanceFix(reason: string | null): Fix {
  const r = reason ?? "";
  if (r.includes("is not set up")) {
    return {
      blocked: false,
      problem: "No instance identity is configured.",
      action: "Set up Service Identity",
    };
  }
  if (r.includes("has no attached account")) {
    return {
      blocked: false,
      problem:
        "Service Identity is set to use an attached account, but none is attached.",
      action: "Attach an account",
    };
  }
  for (const [needle, kind] of [
    ["is did:web", "is a did:web, which has"],
    ["is did:plc", "is a did:plc, which has"],
    ["is not exposed", "isn't exposed, so it has"],
  ] as const) {
    if (r.includes(needle)) {
      return {
        blocked: false,
        problem: `The instance identity ${kind} no repo. Attach an account instead.`,
        action: "Open Service Identity",
      };
    }
  }
  if (r.includes("needs to sign in again")) {
    return {
      blocked: true,
      problem: "The attached account needs to sign in again.",
      action: "Sign in again",
    };
  }
  if (r.includes("hasn't granted permission")) {
    return {
      blocked: true,
      problem:
        "The attached account hasn't granted permission to write lexicon records.",
      action: "Re-authenticate",
    };
  }
  return {
    blocked: true,
    problem: asSentence(reason),
    action: "Open Service Identity",
  };
}

function linkedRepoProblem(reason: string | null): string {
  const r = reason ?? "";
  if (r.includes("not been authorized yet")) {
    return "Not authorized yet.";
  }
  if (r.includes("needs to be reauthorized")) {
    return "Needs to be reauthorized.";
  }
  if (r.includes("doesn't have permission")) {
    return "Doesn't have permission to write lexicon records. Remove it and link it again with the Lexicon publishing preset.";
  }
  if (r.includes("granted less access")) {
    return "Its PDS granted less access than requested. Try linking it again later.";
  }
  return asSentence(reason);
}

/**
 * What stands between this instance and a first publish: either the
 * instance's own account or a linked repo will do, so both are laid out with
 * what each still needs.
 */
export function PublishingSetup({
  targets,
  canSeeLinkedRepos,
  canStillUnpublish,
  className,
}: {
  targets: PublishTarget[];
  canSeeLinkedRepos: boolean;
  /** Some account holds a record HappyView can remove, though none can take a publish. */
  canStillUnpublish: boolean;
  className?: string;
}) {
  const instance = targets.find((t) => t.kind === "instance");
  const repos = targets.filter((t) => t.kind === "linked_repo");
  const fix = instanceFix(instance?.unavailable_reason ?? null);

  return (
    <section
      aria-labelledby="publishing-setup-title"
      className={cn("rounded-md border", className)}
    >
      <div className="flex flex-col gap-1 border-b p-4">
        <h2 id="publishing-setup-title" className="text-base font-medium">
          {canStillUnpublish
            ? "No account can be published to"
            : "Set up an account to publish to"}
        </h2>
        <p className="text-muted-foreground text-sm text-pretty">
          Lexicons are published as records in an account&apos;s repo. Set up
          one of these, then add a publishing rule.
        </p>
      </div>

      <ol className="divide-y">
        <Option
          title="The instance's account"
          blocked={fix.blocked}
          problem={fix.problem}
          action={{ href: SERVICE_IDENTITY_HREF, label: fix.action }}
        />
        <Option
          title="A linked repo"
          blocked={repos.length > 0}
          problem={
            repos.length === 0
              ? "No repos are linked."
              : repos.length === 1
                ? "The linked repo can't be published to."
                : "None of the linked repos can be published to."
          }
          action={
            canSeeLinkedRepos
              ? { href: LINKED_REPOS_HREF, label: "Link a repo" }
              : undefined
          }
        >
          {repos.length > 0 && (
            <ul className="flex flex-col gap-2">
              {repos.map((repo, i) => (
                <li
                  key={repo.linked_repo_id ?? i}
                  className="bg-muted/40 flex min-w-0 flex-col gap-0.5 rounded-sm px-3 py-2"
                >
                  <TargetName target={repo} className="text-sm" />
                  <span className="text-muted-foreground text-xs text-pretty">
                    {isRedacted(repo)
                      ? "You don't have permission to see why."
                      : linkedRepoProblem(repo.unavailable_reason)}
                  </span>
                </li>
              ))}
            </ul>
          )}
          {!canSeeLinkedRepos && (
            <p className="text-muted-foreground text-xs">
              You don&apos;t have permission to link repos.
            </p>
          )}
        </Option>
      </ol>
    </section>
  );
}

function Option({
  title,
  blocked,
  problem,
  action,
  children,
}: {
  title: string;
  blocked: boolean;
  problem: string;
  action?: { href: string; label: string };
  children?: React.ReactNode;
}) {
  const Icon = blocked ? TriangleAlert : CircleDashed;
  return (
    <li className="grid grid-cols-[1rem_minmax(0,1fr)] gap-x-2.5 gap-y-1.5 p-4">
      <Icon
        aria-hidden
        className={cn(
          "mt-0.5 size-4 shrink-0",
          blocked ? "text-warning" : "text-muted-foreground",
        )}
      />
      <p className="text-sm font-medium">{title}</p>
      <div className="col-start-2 flex min-w-0 flex-col items-start gap-3">
        <p className="text-muted-foreground text-sm text-pretty">{problem}</p>
        {children}
        {action && (
          <Button asChild variant="outline" size="sm">
            <Link href={action.href}>
              {action.label}
              <ArrowRight aria-hidden />
            </Link>
          </Button>
        )}
      </div>
    </li>
  );
}
