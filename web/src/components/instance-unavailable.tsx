"use client";

import { Globe, Loader2, WifiOff } from "lucide-react";

import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { docsUrl } from "@/lib/docs";
import type { ConfigFailure } from "@/lib/config-load";

function FullPage({ children }: { children: React.ReactNode }) {
  return (
    <main className="flex min-h-svh items-center justify-center bg-background p-4">
      <div className="w-full max-w-md">{children}</div>
    </main>
  );
}

/** Shown while the first `/config` request is in flight. */
export function InstanceConnecting() {
  return (
    <FullPage>
      <div
        role="status"
        className="flex items-center justify-center gap-2 text-sm text-muted-foreground"
      >
        <Loader2 className="size-4 animate-spin" aria-hidden="true" />
        Connecting to HappyView…
      </div>
    </FullPage>
  );
}

export function InstanceUnavailable({
  failure,
  retrying,
  onRetry,
}: {
  failure: ConfigFailure;
  /** A retry is in flight. */
  retrying: boolean;
  /** Absent when retrying cannot help. */
  onRetry?: () => void;
}) {
  if (failure.kind === "unknown-host") {
    return (
      <FullPage>
        <Card role="alert">
          <CardHeader className="text-center">
            <div className="mb-4 flex justify-center">
              <div className="flex size-12 items-center justify-center rounded-full bg-muted">
                <Globe
                  className="size-6 text-muted-foreground"
                  aria-hidden="true"
                />
              </div>
            </div>
            <CardTitle>
              This instance doesn’t answer on {failure.host}
            </CardTitle>
            <CardDescription>
              HappyView serves only the domains it has registered, and{" "}
              {failure.host} isn’t one of them.
            </CardDescription>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            {failure.isLocalhost ? (
              <p>
                For local development, open the dashboard at{" "}
                <code>127.0.0.1</code> instead of <code>localhost</code>, with
                the same port.{" "}
                <a
                  className="underline underline-offset-4"
                  href={docsUrl(
                    "/getting-started/deployment/local-development",
                  )}
                >
                  Why 127.0.0.1
                </a>
              </p>
            ) : (
              <p>
                Open it at its public URL, or ask its admin to add this domain.
              </p>
            )}
          </CardContent>
        </Card>
      </FullPage>
    );
  }

  return (
    <FullPage>
      <Card role="alert">
        <CardHeader className="text-center">
          <div className="mb-4 flex justify-center">
            <div className="flex size-12 items-center justify-center rounded-full bg-muted">
              <WifiOff
                className="size-6 text-muted-foreground"
                aria-hidden="true"
              />
            </div>
          </div>
          <CardTitle>Can’t reach this HappyView instance</CardTitle>
          <CardDescription>
            The server may still be starting, or it may be down. The dashboard
            keeps trying and opens as soon as it answers.
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-col items-center gap-3">
          {onRetry && (
            <Button onClick={onRetry} disabled={retrying}>
              {retrying && (
                <Loader2 className="size-4 animate-spin" aria-hidden="true" />
              )}
              {retrying ? "Trying…" : "Try again now"}
            </Button>
          )}
          <p className="text-xs text-muted-foreground">{failure.detail}</p>
        </CardContent>
      </Card>
    </FullPage>
  );
}
