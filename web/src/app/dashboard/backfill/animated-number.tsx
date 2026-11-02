"use client";

import { useEffect, useRef, useState } from "react";

/**
 * One frame of the count-up: a sixteenth of the way to `target`, or the
 * target itself once within half a unit, so the shown number settles on it.
 */
export function stepToward(current: number, target: number): number {
  const diff = target - current;
  if (Math.abs(diff) < 0.5) return target;
  return current + diff * 0.06;
}

/** A count that climbs to each new value rather than jumping to it. */
export function AnimatedNumber({ value }: { value: number }) {
  const targetRef = useRef(value);
  const displayedRef = useRef(value);
  const [displayed, setDisplayed] = useState(value);
  const rafRef = useRef<number>(0);

  useEffect(() => {
    targetRef.current = value;
    cancelAnimationFrame(rafRef.current);

    function tick() {
      const next = stepToward(displayedRef.current, targetRef.current);
      displayedRef.current = next;
      setDisplayed(next);
      if (next !== targetRef.current) {
        rafRef.current = requestAnimationFrame(tick);
      }
    }

    tick();
    return () => cancelAnimationFrame(rafRef.current);
  }, [value]);

  return <>{Math.round(displayed).toLocaleString()}</>;
}
