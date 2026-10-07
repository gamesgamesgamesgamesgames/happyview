"use client";

import Link from "next/link";

import { useCurrentUser } from "@/hooks/use-current-user";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";

/** Shown in place of the space moderation pages while the inspector is off. */
export function InspectorDisabled({ title }: { title: string }) {
  const { hasPermission } = useCurrentUser();

  return (
    <>
      <SiteHeader title={title} />
      <div className="flex flex-1 flex-col items-start gap-3 p-4 md:p-6">
        <p className="text-muted-foreground text-sm">
          The space inspector is turned off on this instance.
        </p>
        {hasPermission("settings:manage") && (
          <Button asChild variant="outline" size="sm">
            <Link href="/dashboard/settings/general/">Open settings</Link>
          </Button>
        )}
      </div>
    </>
  );
}
