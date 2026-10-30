"use client";

import { useEffect, useState } from "react";

import { getLimits } from "@/lib/api";
import { limitRows, type LimitRow } from "@/lib/limits";

/**
 * What this instance's plan allows, and how much of it is in use. Renders
 * nothing when the plan sets no limits, which is every self-hosted instance,
 * or when the limits cannot be read: the settings page works without them.
 */
export function PlanLimits() {
  const [rows, setRows] = useState<LimitRow[]>([]);

  useEffect(() => {
    let current = true;
    getLimits()
      .then((limits) => {
        if (current) setRows(limitRows(limits));
      })
      .catch(() => {
        /* nothing to show */
      });
    return () => {
      current = false;
    };
  }, []);

  if (rows.length === 0) return null;
  return (
    <section className="flex flex-col gap-3" aria-labelledby="plan-limits">
      <div>
        <h2 id="plan-limits" className="text-lg font-semibold">
          Plan limits
        </h2>
        <p className="text-muted-foreground text-sm">
          Set by this instance&apos;s hosting plan.
        </p>
      </div>
      <dl className="grid gap-2 sm:grid-cols-3">
        {rows.map((row) => (
          <div key={row.limit} className="rounded-md border p-3">
            <dt className="text-muted-foreground text-xs">{row.label}</dt>
            <dd className="text-sm font-medium">{row.value}</dd>
            {row.over && (
              <dd className="text-destructive text-xs">
                Over your plan&apos;s limit
              </dd>
            )}
          </div>
        ))}
      </dl>
    </section>
  );
}
