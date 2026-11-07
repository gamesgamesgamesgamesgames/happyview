"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { Info, X } from "lucide-react";

import {
  ADDED_PARAM,
  backfillHref,
  suggestBackfill,
} from "@/lib/backfill-hint";
import { Button } from "@/components/ui/button";

/**
 * After a record script is added, suggests a backfill: records already here
 * came in before the script existed, and it doesn't run over them until one
 * does. Shown on the page the save lands on, which it reaches with the
 * script's collection in `?added=`. Records are counted once, and the param
 * is dropped from the URL at once, so a reload doesn't bring the note back;
 * the note itself stays until it is dismissed or the page is left.
 */
export function BackfillHint() {
  const searchParams = useSearchParams();
  // Read once: the param is dropped from the URL below.
  const [collection] = useState(() => searchParams.get(ADDED_PARAM));
  const [shown, setShown] = useState(false);
  const asked = useRef(false);

  useEffect(() => {
    if (!collection || asked.current) return;
    asked.current = true;
    const url = new URL(window.location.href);
    url.searchParams.delete(ADDED_PARAM);
    window.history.replaceState(null, "", url);
    suggestBackfill(collection).then(setShown);
  }, [collection]);

  if (!collection || !shown) return null;
  return (
    <div className="flex items-start gap-3 rounded-lg border bg-muted/50 p-3">
      <Info className="size-4 text-muted-foreground shrink-0 mt-0.5" />
      <p className="flex-1 text-xs">
        <Link href={backfillHref(collection)} className="underline font-medium">
          Start a backfill
        </Link>{" "}
        to run this script over records already here.
      </p>
      <Button
        variant="ghost"
        size="icon-xs"
        aria-label="Dismiss"
        className="-my-1"
        onClick={() => setShown(false)}
      >
        <X />
      </Button>
    </div>
  );
}
