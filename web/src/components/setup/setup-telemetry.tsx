"use client";

import { useState } from "react";

import { updateTelemetry, type TelemetrySettings } from "@/lib/api";
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
      // Only write when the operator chose something other than the default —
      // "off" is already the absence of a setting, and writing it would mint
      // nothing but a row.
      if (mode !== "off") await updateTelemetry({ mode });
    } finally {
      setSaving(false);
      // A telemetry failure must never block setup.
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
