"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { toast } from "sonner";

import {
  getTelemetry,
  getTelemetryPreview,
  sendTelemetry,
  updateTelemetry,
  type TelemetryBenchmarks,
  type TelemetrySettings,
  type TelemetryUpdate,
} from "@/lib/api";
import { toastError } from "@/lib/format";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Switch } from "@/components/ui/switch";

// Matches `MAX_CONTACT_LEN` in `src/admin/telemetry.rs` — enforced here too so
// an operator hits the limit while typing, not as a 400 after committing.
const MAX_CONTACT_LEN = 512;

const MODES = [
  {
    value: "off" as const,
    label: "Don't send anything",
    hint: "No telemetry leaves this instance. This is the default.",
  },
  {
    value: "manual" as const,
    label: "Review each report before sending",
    hint: "Nothing is sent automatically. You press the button below when you want to share.",
  },
  {
    value: "auto" as const,
    label: "Send automatically, once a day",
    hint: "The same payload shown below, once every 24 hours.",
  },
];

const LEXICON_TOGGLES = [
  {
    key: "lexicon_names" as const,
    label: "Lexicon names",
    hint: "The NSIDs of the lexicons and collections on this instance. A custom NSID often names your app.",
  },
  {
    key: "lexicon_structure" as const,
    label: "Lexicon structure",
    hint: "Counts only: which definition types appear, nesting depth, field counts, which primitives are used. No field names, no descriptions.",
  },
  {
    key: "lexicon_documents" as const,
    label: "Full lexicon documents",
    hint: "The complete schema JSON. This is the largest ask on this page — for an unreleased product, your schema may be the most sensitive thing you have.",
  },
];

export default function TelemetryPage() {
  const [settings, setSettings] = useState<TelemetrySettings | null>(null);
  const [preview, setPreview] = useState<string>("");
  const [benchmarks, setBenchmarks] = useState<TelemetryBenchmarks | null>(null);
  const [sending, setSending] = useState(false);
  const [contactDraft, setContactDraft] = useState("");
  // Only seed the draft from the first load. Re-syncing on every `settings`
  // update would overwrite an in-progress edit the moment an unrelated
  // control (a lexicon switch, the mode radio) patches and refetches.
  const contactSeeded = useRef(false);

  const refresh = useCallback(async () => {
    try {
      const [next, payload] = await Promise.all([
        getTelemetry(),
        getTelemetryPreview(),
      ]);
      setSettings(next);
      setPreview(JSON.stringify(payload, null, 2));
      if (!contactSeeded.current) {
        setContactDraft(next.contact ?? "");
        contactSeeded.current = true;
      }
    } catch (e) {
      toastError("Failed to load telemetry settings", e);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const patch = useCallback(async (body: TelemetryUpdate) => {
    try {
      setSettings(await updateTelemetry(body));
      // The payload changes with the toggles, so re-read it rather than
      // leaving a stale document on screen next to a switch that moved.
      setPreview(JSON.stringify(await getTelemetryPreview(), null, 2));
    } catch (e) {
      toastError("Failed to update telemetry settings", e);
    }
  }, []);

  const commitContact = useCallback(() => {
    const trimmed = contactDraft.trim();
    // Nothing to send if it matches what the server already has — every
    // other keystroke isn't a save, so a blur that changed nothing shouldn't
    // be one either.
    if (trimmed === (settings?.contact ?? "")) return;
    void patch({ contact: trimmed });
  }, [contactDraft, settings, patch]);

  const send = useCallback(async () => {
    setSending(true);
    try {
      const result = await sendTelemetry();
      setBenchmarks(result.benchmarks);
      toast.success(
        result.benchmarks
          ? "Sent. Comparison updated below."
          : "Sent. Not enough comparable instances yet for a comparison.",
      );
    } catch {
      // Never surface the raw error: it can carry a collector hostname or an
      // internal message, and the operator only needs to know it did not land.
      toast.error("Couldn't reach the telemetry collector. Nothing was sent.");
    } finally {
      setSending(false);
    }
  }, []);

  const mode = settings?.mode ?? "off";
  const sharing = mode !== "off";

  return (
    <>
      <SiteHeader title="Telemetry" />
      <div className="flex flex-col gap-4 p-4 lg:p-6 max-w-3xl">
        <Card>
          <CardHeader>
            <CardTitle>What this instance shares</CardTitle>
            <CardDescription>
              Off by default. Everything below is opt-in, each part separately,
              and you can read the exact payload before you turn anything on.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            <RadioGroup
              value={mode}
              onValueChange={(value) =>
                void patch({ mode: value as TelemetrySettings["mode"] })
              }
            >
              {MODES.map((m) => (
                <div key={m.value} className="flex items-start gap-3">
                  <RadioGroupItem value={m.value} id={`mode-${m.value}`} />
                  <div className="grid gap-1">
                    <Label htmlFor={`mode-${m.value}`}>{m.label}</Label>
                    <p className="text-muted-foreground text-sm">{m.hint}</p>
                  </div>
                </div>
              ))}
            </RadioGroup>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Contact (optional)</CardTitle>
            <CardDescription>
              If you&apos;d like us to be able to reach you about what we see
              in your reports, leave a way to reach you below. This is
              separate from whether you send anything at all — you can report
              without ever filling this in.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-2">
            <Label htmlFor="contact">Contact</Label>
            <Input
              id="contact"
              placeholder="you@example.com or @handle.bsky.social"
              maxLength={MAX_CONTACT_LEN}
              disabled={!sharing}
              value={contactDraft}
              onChange={(e) => setContactDraft(e.target.value)}
              onBlur={commitContact}
              onKeyDown={(e) => {
                if (e.key === "Enter") e.currentTarget.blur();
              }}
            />
            <p className="text-muted-foreground text-sm">
              Optional, up to {MAX_CONTACT_LEN} characters. Only included in a
              report if you provide it.
            </p>
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Lexicon detail</CardTitle>
            <CardDescription>
              Counts and shapes are always included. These three add detail, and
              each is separate — sharing structure does not share names.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            {LEXICON_TOGGLES.map((t) => (
              <div key={t.key} className="flex items-start justify-between gap-4">
                <div className="grid gap-1">
                  <Label htmlFor={t.key}>{t.label}</Label>
                  <p className="text-muted-foreground text-sm">{t.hint}</p>
                </div>
                <Switch
                  id={t.key}
                  aria-label={t.label}
                  disabled={!sharing}
                  checked={settings?.[t.key] ?? false}
                  onCheckedChange={(checked) => void patch({ [t.key]: checked })}
                />
              </div>
            ))}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Exactly what would be sent</CardTitle>
            <CardDescription>
              Assembled from this instance, right now, under the settings above.
              Nothing outside this document leaves your server.
            </CardDescription>
          </CardHeader>
          <CardContent className="flex flex-col gap-4">
            <div className="text-muted-foreground flex flex-wrap items-center gap-2 text-sm">
              <span>Sent to</span>
              <code className="bg-muted rounded px-1.5 py-0.5 font-mono text-xs">
                {settings?.collector_url}
              </code>
            </div>
            <pre
              data-testid="telemetry-preview"
              className="bg-muted max-h-96 overflow-auto rounded p-3 text-xs"
            >
              {preview}
            </pre>

            {mode === "manual" && (
              <div className="flex flex-col gap-2">
                <Button onClick={() => void send()} disabled={sending}>
                  {sending ? "Sending…" : "Send and compare"}
                </Button>
                <p className="text-muted-foreground text-sm">
                  Counters are lifetime totals, so sending once in a while still
                  gives an accurate picture — you lose resolution, not accuracy.
                </p>
              </div>
            )}

            {benchmarks && (
              <p className="text-sm">
                Compared against {benchmarks.cohort_size} instances of a similar
                size.
              </p>
            )}
          </CardContent>
        </Card>
      </div>
    </>
  );
}
