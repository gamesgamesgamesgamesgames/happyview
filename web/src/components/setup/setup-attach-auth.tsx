"use client";

import { useEffect, useState } from "react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { confirmAttachAuth } from "@/lib/api";
import { docsUrl } from "@/lib/docs";

const ATTACH_AUTH_STORAGE_KEY = "happyview_attach_auth";
const ATTACH_AUTH_MAX_AGE_MS = 10 * 60 * 1000;

interface AttachAuthPayload {
  attachedDid: string;
  originalDid: string;
  timestamp?: number;
}

interface SetupAttachAuthProps {
  attachedDid: string;
  attachedHandle: string | null;
  onComplete: () => void;
  onBack?: () => void;
}

export function SetupAttachAuth({
  attachedDid,
  attachedHandle,
  onComplete,
  onBack,
}: SetupAttachAuthProps) {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const stored = localStorage.getItem(ATTACH_AUTH_STORAGE_KEY);
    if (!stored) return;

    let payload: AttachAuthPayload;
    try {
      payload = JSON.parse(stored) as AttachAuthPayload;
    } catch {
      localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY);
      return;
    }

    if (payload.attachedDid !== attachedDid) {
      localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY);
      return;
    }

    if (
      payload.timestamp &&
      Date.now() - payload.timestamp > ATTACH_AUTH_MAX_AGE_MS
    ) {
      localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY);
      setError("Your sign-in session expired. Please authenticate again.");
      return;
    }

    localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY);
    setConfirming(true);

    confirmAttachAuth({ original_did: payload.originalDid })
      .then(() => onComplete())
      .catch((e) => {
        setError(
          e instanceof Error
            ? e.message
            : "Failed to restore admin session. Try authenticating again.",
        );
        setConfirming(false);
      });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  function handleAuthenticate() {
    setConfirming(true);
    setError(null);

    fetch("/auth/me", { credentials: "same-origin" })
      .then((res) => {
        if (!res.ok) throw new Error("Failed to fetch current user");
        return res.json() as Promise<{ did: string }>;
      })
      .then(({ did: originalDid }) => {
        const payload: AttachAuthPayload = {
          attachedDid,
          originalDid,
          timestamp: Date.now(),
        };
        localStorage.setItem(ATTACH_AUTH_STORAGE_KEY, JSON.stringify(payload));

        const handle = attachedHandle ?? attachedDid;
        return fetch(`/auth/login?handle=${encodeURIComponent(handle)}&scope=${encodeURIComponent("atproto identity:*")}`, {
          credentials: "same-origin",
        });
      })
      .then((resp) => {
        if (!resp.ok) throw new Error("Login request failed");
        return resp.json() as Promise<{ url: string }>;
      })
      .then(({ url }) => {
        window.location.href = url;
      })
      .catch((e) => {
        setError(
          e instanceof Error
            ? e.message
            : "Failed to start authentication. Check your connection and try again.",
        );
        setConfirming(false);
      });
  }

  const displayName = attachedHandle ? `@${attachedHandle}` : attachedDid;

  return (
    <Card>
      <CardHeader>
        <CardTitle>Sign in to verify ownership</CardTitle>
        {/* <CardDescription>
          You&apos;ll be redirected to sign in as{" "}
          <span className="font-medium">{displayName}</span>, then returned here
          automatically. <br />
        </CardDescription> */}
      </CardHeader>
      <CardContent className="space-y-4">
        <p className="text-muted-foreground text-sm">
          You&apos;ll leave this page briefly to authenticate through the
          account&apos;s data server. Once verified, your admin session will be
          restored and you&apos;ll continue from where you left off.
          <br />
          <a
            href={docsUrl("/getting-started/service-identity#linked-account")}
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary underline underline-offset-4 hover:text-primary/80"
          >
            <small>Learn more</small>
          </a>
        </p>
        {error && (
          <p role="alert" className="text-destructive text-sm">
            {error}
          </p>
        )}
        {confirming ? (
          <p className="text-muted-foreground text-sm" aria-live="polite">
            Restoring admin session…
          </p>
        ) : (
          <div className="flex justify-between">
            {onBack ? (
              <Button variant="ghost" onClick={onBack}>
                Back
              </Button>
            ) : (
              <div />
            )}
            <Button onClick={handleAuthenticate}>
              Authenticate as {displayName}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
