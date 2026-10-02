"use client";

import { useState } from "react";

import {
  dismissTelemetryPrompt,
  updateTelemetry,
  type TelemetrySettings,
} from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";

const OPTIONS = [
  {
    value: "off" as const,
    label: "Don't send anything",
    hint: "This is the default. You can turn this on later in Settings → Telemetry.",
  },
  {
    value: "manual" as const,
    label: "Let me review each report first",
    hint: "Nothing is sent automatically. You get a button and the full payload to read.",
  },
  {
    value: "auto" as const,
    label: "Send a daily report",
    hint: "One JSON document a day. In return you get to see how your instance compares to others of a similar size.",
  },
];

export function SetupTelemetry({ onNext }: { onNext: () => void }) {
  const [mode, setMode] = useState<TelemetrySettings["mode"]>("off");
  const [saving, setSaving] = useState(false);

  async function submit() {
    setSaving(true);
    try {
      // Only write the mode when the operator chose something other than the
      // default — "off" is already the absence of a setting, and writing it
      // would mint nothing but a row. Declining is still an *answer* though,
      // so it records that the question was asked; otherwise the dashboard
      // prompt, which exists to catch instances that never saw this step,
      // would ask again on the very next page load.
      if (mode === "off") await dismissTelemetryPrompt();
      else await updateTelemetry({ mode });
    } catch {
      // A telemetry failure must never block setup. The `finally` below
      // already guaranteed that, but the rejection still escaped `void
      // submit()` as an unhandled rejection — and the "off" branch, which
      // previously made no request at all, is now the one most likely to
      // reach it.
    } finally {
      setSaving(false);
      onNext();
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Help us build the right things</CardTitle>
        <CardDescription>
          HappyView can send us a daily report about how this instance is
          doing: how much data it holds, which features you use, and where it
          is struggling. It never includes records, handles, DIDs, or the
          contents of anything you have indexed. You can read the exact
          payload — and change your mind — any time under Settings →
          Telemetry.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <RadioGroup
          value={mode}
          onValueChange={(v) => setMode(v as TelemetrySettings["mode"])}
        >
          {OPTIONS.map((o) => (
            <div key={o.value} className="flex items-start gap-3">
              <RadioGroupItem
                value={o.value}
                id={`setup-telemetry-${o.value}`}
              />
              <div className="grid gap-1">
                <Label htmlFor={`setup-telemetry-${o.value}`}>
                  {o.label}
                </Label>
                <p className="text-muted-foreground text-sm">{o.hint}</p>
              </div>
            </div>
          ))}
        </RadioGroup>

        <div className="flex justify-end">
          <Button onClick={() => void submit()} disabled={saving}>
            {saving ? "Saving…" : "Continue"}
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}
