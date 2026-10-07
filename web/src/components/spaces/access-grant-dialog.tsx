"use client";

import { useState } from "react";

import { createAccessGrant } from "@/lib/api";
import { toastError } from "@/lib/format";
import type { AccessGrant, GrantScope } from "@/types/spaces";
import { Button } from "@/components/ui/button";
import { Label } from "@/components/ui/label";
import { Textarea } from "@/components/ui/textarea";
import {
  ResponsiveDialog,
  ResponsiveDialogContent,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
} from "@/components/ui/responsive-dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export interface ScopeOption {
  scope: GrantScope;
  target: string;
  label: string;
}

const DURATIONS = [5, 15, 30, 60, 120, 240, 480];

export function AccessGrantDialog({
  open,
  onOpenChange,
  scopeOptions,
  maxMinutes,
  defaultMinutes,
  onGranted,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  scopeOptions: ScopeOption[];
  maxMinutes: number;
  defaultMinutes: number;
  onGranted: (grant: AccessGrant) => void;
}) {
  const [choice, setChoice] = useState("0");
  const [reason, setReason] = useState("");
  const [minutes, setMinutes] = useState(String(defaultMinutes));
  const [busy, setBusy] = useState(false);

  const durations = Array.from(
    new Set([...DURATIONS.filter((d) => d <= maxMinutes), maxMinutes]),
  ).sort((a, b) => a - b);

  async function submit() {
    const option = scopeOptions[Number(choice)];
    setBusy(true);
    try {
      const grant = await createAccessGrant({
        scope: option.scope,
        target: option.target,
        reason: reason.trim(),
        duration_minutes: Number(minutes),
      });
      setReason("");
      onGranted(grant);
      onOpenChange(false);
    } catch (e: unknown) {
      toastError("Couldn't grant access", e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={onOpenChange}>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Request access</ResponsiveDialogTitle>
        </ResponsiveDialogHeader>
        <div className="flex flex-col gap-4">
          <p className="text-muted-foreground text-sm">
            Your reason, and every record and blob you open, is logged under
            your account. These logs can&apos;t be purged.
          </p>
          {scopeOptions.length > 1 && (
            <div className="flex flex-col gap-2">
              <Label htmlFor="grant-scope">Access to</Label>
              <Select value={choice} onValueChange={setChoice}>
                <SelectTrigger id="grant-scope">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {scopeOptions.map((o, i) => (
                    <SelectItem key={`${o.scope}:${o.target}`} value={String(i)}>
                      {o.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          )}
          <div className="flex flex-col gap-2">
            <Label htmlFor="grant-reason">Reason</Label>
            <Textarea
              id="grant-reason"
              value={reason}
              maxLength={2000}
              onChange={(e) => setReason(e.target.value)}
              placeholder="Report #123: harassment in a thread"
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="grant-duration">Duration</Label>
            <Select value={minutes} onValueChange={setMinutes}>
              <SelectTrigger id="grant-duration">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {durations.map((d) => (
                  <SelectItem key={d} value={String(d)}>
                    {d < 60 ? `${d} minutes` : `${d / 60} hour${d === 60 ? "" : "s"}`}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="flex justify-end gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)}>
              Cancel
            </Button>
            <Button onClick={submit} disabled={busy || reason.trim() === ""}>
              {busy ? "Granting…" : "Grant access"}
            </Button>
          </div>
        </div>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
