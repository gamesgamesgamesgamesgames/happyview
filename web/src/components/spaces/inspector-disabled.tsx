"use client";

import { SiteHeader } from "@/components/site-header";

/** Shown in place of the space moderation pages while the inspector is off. */
export function InspectorDisabled({ title }: { title: string }) {
  return (
    <>
      <SiteHeader title={title} />
      <div className="flex flex-1 flex-col items-start gap-3 p-4 md:p-6">
        <p className="text-muted-foreground text-sm">
          The space inspector is turned off on this instance.
        </p>
      </div>
    </>
  );
}
