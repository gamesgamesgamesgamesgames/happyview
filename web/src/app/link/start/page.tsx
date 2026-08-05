"use client";

import { Suspense, useEffect, useState } from "react";
import Image from "next/image";
import { useSearchParams } from "next/navigation";
import { AlertTriangle, Loader2 } from "lucide-react";

import { getLinkedRepoInvite, type LinkedRepoInviteInfo } from "@/lib/api";
import { describeScope } from "@/lib/linked-repo-scopes";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "@/components/ui/empty";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Skeleton } from "@/components/ui/skeleton";

const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH || "";

function LinkStartInner() {
  const searchParams = useSearchParams();
  const token = searchParams.get("token") ?? "";

  const [info, setInfo] = useState<LinkedRepoInviteInfo | null>(null);
  // Distinguishes "the server told us this token is dead" from "we couldn't
  // even reach the server" — the former has no useful retry, the latter
  // might just be a network blip.
  const [fetchFailed, setFetchFailed] = useState(false);
  const [handle, setHandle] = useState("");

  const [retryCount, setRetryCount] = useState(0);

  useEffect(() => {
    if (!token) return;
    let cancelled = false;
    // Deliberately no synchronous setState reset here — React's compiler flags
    // that as a cascading render. The retry button clears `fetchFailed` itself
    // before bumping `retryCount`, which is the only way this effect re-runs.
    getLinkedRepoInvite(token)
      .then((data) => {
        if (!cancelled) setInfo(data);
      })
      .catch(() => {
        if (!cancelled) setFetchFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [token, retryCount]);

  function handleContinue() {
    if (!info) return;
    const params = new URLSearchParams({ token });
    const chosenHandle = info.pinned_identifier ?? handle.trim();
    if (chosenHandle) params.set("handle", chosenHandle);
    window.location.assign(
      `${BASE_PATH}/auth/linked-repo/start?${params.toString()}`,
    );
  }

  // No token at all — this link can never resolve to anything, so treat it
  // the same as a token the server has already rejected.
  if (!token || (info && !info.valid)) {
    return (
      <Empty className="border">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <AlertTriangle className="text-yellow-600 dark:text-yellow-500" />
          </EmptyMedia>
          <EmptyTitle role="heading" aria-level={1}>
            This link is no longer usable
          </EmptyTitle>
          <EmptyDescription>
            It may have expired, already been used, or been revoked. Ask whoever
            sent it to you for a new one — retrying this one won&apos;t help.
          </EmptyDescription>
        </EmptyHeader>
      </Empty>
    );
  }

  if (fetchFailed) {
    return (
      <Empty className="border">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <AlertTriangle className="text-destructive" />
          </EmptyMedia>
          <EmptyTitle role="heading" aria-level={1}>
            Couldn&apos;t load this link
          </EmptyTitle>
          <EmptyDescription>
            Something went wrong reaching the server. Check your connection and
            try again.
          </EmptyDescription>
        </EmptyHeader>
        <Button
          variant="outline"
          onClick={() => {
            setFetchFailed(false);
            setRetryCount((n) => n + 1);
          }}
        >
          Try again
        </Button>
      </Empty>
    );
  }

  if (!info) {
    return (
      <Card className="w-full">
        <CardHeader>
          <div className="flex items-center gap-3">
            <Skeleton className="size-10 rounded-md" />
            <div className="flex flex-col gap-2">
              <Skeleton className="h-4 w-32" />
              <Skeleton className="h-3 w-48" />
            </div>
          </div>
        </CardHeader>
        <CardContent className="flex flex-col gap-3">
          <Skeleton className="h-16 w-full" />
          <Skeleton className="h-16 w-full" />
        </CardContent>
      </Card>
    );
  }

  const descriptions = info.scopes.map(describeScope);
  const capabilities = descriptions.filter((d) => !d.quiet);
  const hasBasicAccess = descriptions.some((d) => d.quiet);
  const canContinue =
    Boolean(info.pinned_identifier) || handle.trim().length > 0;

  return (
    <Card className="w-full">
      <CardHeader>
        <div className="flex items-center gap-3">
          {info.logo_url && (
            <Image
              src={info.logo_url}
              alt=""
              width={40}
              height={40}
              unoptimized
              className="rounded-md object-contain shrink-0"
            />
          )}
          <div className="flex flex-col gap-1">
            <CardTitle className="text-xl">{info.app_name}</CardTitle>
            <CardDescription>
              is asking for access to your ATProto repo
            </CardDescription>
          </div>
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-6">
        {info.reason && (
          <div className="flex flex-col gap-1.5">
            <h2 className="text-sm font-medium">Reason</h2>
            {/* Stacked rather than inline: this is the admin's own prose and can
                run to several lines, so it needs room to breathe. `whitespace-
                pre-line` keeps any line breaks they typed. */}
            <p className="rounded-md border bg-muted/30 px-3 py-2 text-sm whitespace-pre-line">
              {info.reason}
            </p>
          </div>
        )}

        <div className="flex flex-col gap-3">
          <h2 className="text-sm font-medium">
            What this will let {info.app_name} do
          </h2>
          {capabilities.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              No specific record or file access was requested.
            </p>
          ) : (
            <ul className="flex flex-col gap-2">
              {capabilities.map((cap) => (
                <li
                  key={cap.raw}
                  className="flex flex-col gap-0.5 rounded-md border px-3 py-2"
                >
                  <span className="text-sm">{cap.text}</span>
                  <span className="font-mono text-xs text-muted-foreground break-all">
                    {cap.raw}
                  </span>
                </li>
              ))}
            </ul>
          )}
          {hasBasicAccess && (
            <p className="text-muted-foreground text-xs">
              Basic account access is also included — that only identifies which
              account is linked, it isn&apos;t a separate permission.
            </p>
          )}
        </div>

        <div className="flex flex-col gap-2">
          {info.pinned_identifier ? (
            <>
              <Label>Account</Label>
              <p className="rounded-md border bg-muted/30 px-3 py-2 font-mono text-sm">
                {info.pinned_identifier}
              </p>
              <p className="text-muted-foreground text-xs">
                This link is only valid for this account.
              </p>
            </>
          ) : (
            <>
              <Label htmlFor="handle">Your handle</Label>
              <Input
                id="handle"
                value={handle}
                onChange={(e) => setHandle(e.target.value)}
                placeholder="you.bsky.social"
                autoComplete="off"
              />
            </>
          )}
        </div>

        {info.expires_at && (
          <p className="text-muted-foreground text-xs">
            This link expires {new Date(info.expires_at).toLocaleString()}.
          </p>
        )}

        <p className="text-muted-foreground text-xs">
          Continuing takes you to your own PDS to approve this. {info.app_name}{" "}
          never sees your password.
        </p>
      </CardContent>
      <CardFooter>
        <Button
          className="w-full"
          onClick={handleContinue}
          disabled={!canContinue}
        >
          Continue
        </Button>
      </CardFooter>
    </Card>
  );
}

function LinkStartFallback() {
  return (
    <Card className="w-full">
      <CardContent className="flex items-center justify-center gap-2 py-10 text-muted-foreground">
        <Loader2 className="size-4 animate-spin" />
        <span className="text-sm">Loading…</span>
      </CardContent>
    </Card>
  );
}

export default function LinkStartPage() {
  return (
    <div className="bg-background flex min-h-svh flex-col items-center justify-center gap-6 p-6 md:p-10">
      <div className="w-full max-w-md">
        <Suspense fallback={<LinkStartFallback />}>
          <LinkStartInner />
        </Suspense>
      </div>
    </div>
  );
}
