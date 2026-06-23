"use client";

import { useState } from "react";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { docsUrl } from "@/lib/docs";
import { HelpTip } from "./help-tip";

const IDENTITY_MODES = [
  {
    value: "did_web",
    title: "Use your domain",
    description:
      "Your domain name becomes your identity. This is the simplest option, since HappyView will generate everything automatically.",
    helpTip:
      "Uses your domain as a did:web identifier. Your server hosts a DID document at /.well-known/did.json. The identity is tied to your domain.",
    badge: "Recommended",
  },
  {
    value: "attach_account",
    title: "Use an existing AT Protocol account",
    description:
      "Link this AppView to an account you already own. You'll verify ownership through that account.",
    helpTip:
      "Links your AppView to an existing account's DID. Authentication goes through that account's Personal Data Server.",
    badge: null,
  },
  {
    value: "did_plc",
    title: "Create a new network identity",
    description: (
      <>
        Register a new identity in the AT Protocol directory. This is the most
        durable option because a <code>did:plc</code> will survive domain
        changes.
      </>
    ),
    helpTip:
      "Registers a did:plc identity in the AT Protocol directory. Supports key rotation and recovery, and isn't tied to any single domain.",
    badge: null,
  },
];

interface SetupIdentityModeProps {
  onComplete: (mode: string) => void | Promise<void>;
}

export function SetupIdentityMode({ onComplete }: SetupIdentityModeProps) {
  const [selected, setSelected] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  return (
    <Card>
      <CardHeader>
        <CardTitle>Set up your service identity</CardTitle>
        <CardDescription>
          AT Protocol apps typically verify requests through a user&apos;s data
          server before they reach your AppView. To accept those requests, your
          AppView needs its own identity on the network.
          <br />
          <br />
          <strong>This is optional.</strong> HappyView includes its own auth,
          but a service identity is recommended for compatibility with standard
          AT Protocol apps.
          <br />
          <a
            href={docsUrl("/getting-started/service-identity")}
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary underline underline-offset-4 hover:text-primary/80"
          >
            <small>Learn more</small>
          </a>
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div role="radiogroup" aria-label="Identity mode" className="space-y-3">
          {IDENTITY_MODES.map((mode) => (
            <button
              key={mode.value}
              type="button"
              role="radio"
              aria-checked={selected === mode.value}
              className={cn(
                "w-full rounded-lg border-2 p-4 text-left transition-colors",
                selected === mode.value
                  ? "border-primary bg-primary/5"
                  : "border-border hover:border-primary/50",
              )}
              onClick={() => setSelected(mode.value)}
            >
              <div className="flex items-center gap-2">
                <span className="font-semibold">{mode.title}</span>
                <HelpTip
                  label={mode.helpTip}
                  href={docsUrl("/getting-started/service-identity")}
                />
                {mode.badge && (
                  <span className="rounded-full bg-primary/10 px-2 py-0.5 text-xs font-medium text-primary">
                    {mode.badge}
                  </span>
                )}
              </div>
              <div className="text-muted-foreground text-sm mt-1">
                {mode.description}
              </div>
            </button>
          ))}

          <div className="border-t pt-4" role="none">
            <button
              type="button"
              role="radio"
              aria-checked={selected === "not_exposed"}
              className={cn(
                "w-full rounded-lg border-2 p-4 text-left transition-colors",
                selected === "not_exposed"
                  ? "border-primary bg-primary/5"
                  : "border-border hover:border-primary/50",
              )}
              onClick={() => setSelected("not_exposed")}
            >
              <div className="font-semibold">Skip for now</div>
              <div className="text-muted-foreground text-sm mt-1">
                Come back to this in settings whenever you&apos;re ready. Your
                AppView will work with HappyView&apos;s built-in auth, but
                standard AT Protocol app routing won&apos;t be available.
              </div>
            </button>
          </div>
        </div>

        <div className="flex justify-end pt-2">
          <Button
            disabled={!selected || submitting}
            onClick={async () => {
              if (!selected) return;
              setSubmitting(true);
              try {
                await onComplete(selected);
              } catch {
                setSubmitting(false);
              }
            }}
          >
            {submitting ? "Configuring…" : "Continue"}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
