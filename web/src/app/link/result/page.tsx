"use client";

import { Suspense } from "react";
import { useSearchParams } from "next/navigation";
import { AlertTriangle, Ban, CheckCircle2, Clock, XCircle } from "lucide-react";

import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";

type ResultStatus =
  | "success"
  | "expired"
  | "mismatch"
  | "already_linked"
  | "failed"
  | "gone";

interface ResultCopy {
  icon: React.ComponentType<{ className?: string }>;
  iconClassName: string;
  title: string;
  description: (handle: string | null) => React.ReactNode;
}

const RESULT_COPY: Record<ResultStatus, ResultCopy> = {
  success: {
    icon: CheckCircle2,
    iconClassName: "text-green-600 dark:text-green-500",
    title: "Repo linked",
    description: (handle) => (
      <>
        {handle ? (
          <>
            <span className="font-mono">{handle}</span> is now linked.
          </>
        ) : (
          "Your repo is now linked."
        )}{" "}
        You can review or revoke this access at any time from your own
        account or PDS settings.
      </>
    ),
  },
  expired: {
    icon: Clock,
    iconClassName: "text-yellow-600 dark:text-yellow-500",
    title: "This link expired",
    description: () =>
      "The authorization took too long to complete, or it was already finished elsewhere. Ask whoever sent you the link for a fresh one.",
  },
  mismatch: {
    icon: AlertTriangle,
    iconClassName: "text-yellow-600 dark:text-yellow-500",
    title: "Wrong account authorized",
    description: () =>
      "You approved this from a different account than the one this link was issued for. Make sure you're signed into the correct account, then open the link again to retry.",
  },
  already_linked: {
    icon: CheckCircle2,
    iconClassName: "text-muted-foreground",
    title: "Already linked",
    description: () =>
      "This repo is already linked. There's nothing more you need to do.",
  },
  failed: {
    icon: XCircle,
    iconClassName: "text-destructive",
    title: "Something went wrong",
    description: () =>
      "We couldn't complete the connection. Try opening the link again, and if it keeps failing, contact whoever sent it to you.",
  },
  gone: {
    icon: Ban,
    iconClassName: "text-muted-foreground",
    title: "Request withdrawn",
    description: () =>
      "Whoever sent this link canceled the request before you finished. No action is needed on your end — reach out to them if you still need to grant access.",
  },
};

function isResultStatus(value: string | null): value is ResultStatus {
  return value !== null && value in RESULT_COPY;
}

function LinkResultInner() {
  const searchParams = useSearchParams();
  const statusParam = searchParams.get("status");
  const handle = searchParams.get("handle");

  const status: ResultStatus = isResultStatus(statusParam) ? statusParam : "failed";
  const copy = RESULT_COPY[status];
  const Icon = copy.icon;

  return (
    <Empty className="border">
      <EmptyHeader>
        <EmptyMedia variant="icon">
          <Icon className={copy.iconClassName} />
        </EmptyMedia>
        {/* `EmptyTitle` renders a plain div. On a standalone page a stranger
            lands on cold, this is the page's title and should be exposed as a
            heading to assistive tech. */}
        <EmptyTitle role="heading" aria-level={1}>
          {copy.title}
        </EmptyTitle>
        <EmptyDescription>{copy.description(handle)}</EmptyDescription>
      </EmptyHeader>
      {status === "mismatch" && (
        <EmptyContent>
          <p className="text-muted-foreground text-sm">
            No action was taken on the account you meant to link — it&apos;s safe
            to try again.
          </p>
        </EmptyContent>
      )}
    </Empty>
  );
}

export default function LinkResultPage() {
  return (
    <div className="bg-background flex min-h-svh flex-col items-center justify-center gap-6 p-6 md:p-10">
      <div className="w-full max-w-md">
        <Suspense fallback={null}>
          <LinkResultInner />
        </Suspense>
      </div>
    </div>
  );
}
