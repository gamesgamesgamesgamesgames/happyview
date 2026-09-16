"use client";

import { useEffect } from "react";
import { useRouter } from "next/navigation";
import { toast } from "sonner";

import { dismissTelemetryPrompt, getTelemetry } from "@/lib/api";

const TOAST_ID = "telemetry-prompt";

/**
 * Asks — once, ever — whether the operator wants to send usage reports.
 *
 * The setup wizard poses this question, so instances created since it exists
 * arrive already answered. Older ones never saw it at all, and would otherwise
 * go their whole life without being asked. This is the catch-up path for them.
 *
 * "Never asked" is a server-side fact (`prompted`), not a per-browser one: the
 * answer is instance-wide, so dismissing it here has to silence it for every
 * admin in every browser. That is why the dismissal is written back through
 * the API rather than to localStorage, unlike the vacuum prompt next door.
 *
 * Renders nothing, and holds no context — mounted as a leaf beside the
 * Toaster rather than wrapped around the tree.
 */
export function TelemetryPrompt() {
  const router = useRouter();

  // Mount-only. Unlike the vacuum prompt there is nothing to poll for: the
  // answer changes exactly once, and only in response to something the
  // operator does in this very session.
  useEffect(() => {
    let cancelled = false;

    async function ask() {
      let settings;
      try {
        settings = await getTelemetry();
      } catch {
        return; // best-effort; the user may lack settings:manage
      }
      if (cancelled || settings.prompted) return;

      // Stamping it server-side is what makes "never again" stick. If the
      // write fails the toast simply returns on the next load, which is the
      // right way to fail — an unanswered question stays unanswered.
      const answer = () => {
        void dismissTelemetryPrompt().catch(() => {});
      };

      // No timeout, and a close button, matching VacuumPromptProvider: this is
      // a question rather than a confirmation, and a ~4s toast is one an
      // operator can miss entirely. Since it is only ever shown once, missing
      // it means never being asked.
      toast("Help shape HappyView", {
        id: TOAST_ID,
        duration: Infinity,
        closeButton: true,
        description:
          "Anonymous usage reports tell us which features to keep building. No records, handles, or DIDs are ever included.",
        onDismiss: answer,
        action: {
          label: "Review",
          onClick: () => {
            answer();
            toast.dismiss(TOAST_ID);
            router.push("/dashboard/settings/telemetry");
          },
        },
      });
    }

    void ask();
    return () => {
      cancelled = true;
    };
  }, [router]);

  return null;
}
