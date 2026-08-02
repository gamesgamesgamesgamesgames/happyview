import { toast } from "sonner";

export function toastError(context: string, e: unknown) {
  const msg = e instanceof Error ? e.message : String(e);
  const lower = msg.toLowerCase();
  if (lower.includes("unique") || lower.includes("duplicate") || lower.includes("already exists")) {
    toast.error(`${context}: already exists`, {
      description: "An entry with this identifier is already configured.",
    });
    return;
  }
  if (lower.includes("network") || lower.includes("fetch") || lower.includes("econnrefused")) {
    toast.error(`${context}: connection failed`, {
      description: "Check that the server is running and try again.",
    });
    return;
  }
  if (lower.includes("unauthorized") || lower.includes("403") || lower.includes("forbidden")) {
    toast.error(`${context}: permission denied`, {
      description: "You may not have the required permissions for this action.",
    });
    return;
  }
  if (lower.includes("timeout")) {
    toast.error(`${context}: request timed out`, {
      description: "The server took too long to respond. Try again in a moment.",
    });
    return;
  }
  toast.error(context, { description: msg });
}

// `null` means the measurement failed, not that it measured zero — rendering
// it as "0 B" would send an operator chasing disk space that was never the
// problem, so it gets its own word. `undefined` means the value isn't known
// yet (e.g. still loading).
const BYTE_UNITS = ["B", "KiB", "MiB", "GiB", "TiB"];

/** Scale to a unit, rounding *before* deciding the unit so a value that rounds
 * up to 1024.0 rolls over rather than rendering "1024.0 KiB". */
function scaleBytes(n: number): { value: number; unit: string } {
  let v = n;
  let u = 0;
  while (u < BYTE_UNITS.length - 1) {
    // Compare against the rounded value, or 1023.97 KiB renders as "1024.0 KiB".
    if (Number(v.toFixed(1)) < 1024) break;
    v /= 1024;
    u += 1;
  }
  return { value: v, unit: BYTE_UNITS[u] };
}

export function formatBytes(n: number | null | undefined): string {
  if (n === null) return "unknown";
  if (n === undefined) return "—";
  const { value, unit } = scaleBytes(n);
  return unit === "B" ? `${n} B` : `${value.toFixed(1)} ${unit}`;
}

/**
 * Same value, split so a column of them lines up: `pad` is the leading zeros
 * that bring the integer part to three digits, `rest` is everything else. The
 * unit is right-padded to three characters with non-breaking spaces (regular
 * ones collapse in HTML) so byte-scale values align with MiB/GiB rows.
 *
 * Returned in two pieces rather than one string so the caller can render the
 * zeros muted: they exist only to reserve width, and at full contrast they
 * read as significant digits — worse than the ragged column they fix.
 *
 * Only for tabular display in a monospace cell — never for prose, where
 * "Reclaimed 004.2 GiB" reads as a bug. Use `formatBytes` there.
 *
 * Values of 1000–1023 in a unit come out one character wider; that beats
 * padding every value to four digits for a case this page rarely shows.
 */
export function formatBytesParts(n: number | null | undefined): {
  pad: string;
  rest: string;
} {
  if (n === null) return { pad: "", rest: "unknown" };
  if (n === undefined) return { pad: "", rest: "—" };
  const { value, unit } = scaleBytes(n);
  const [whole, frac = "0"] = value.toFixed(1).split(".");
  return {
    pad: "0".repeat(Math.max(0, 3 - whole.length)),
    rest: `${whole}.${frac} ${unit.padEnd(3, " ")}`,
  };
}

export function formatDate(
  date: Date | string | number | undefined,
  opts: Intl.DateTimeFormatOptions = {},
) {
  if (!date) return "";

  try {
    return new Intl.DateTimeFormat("en-US", {
      month: opts.month ?? "long",
      day: opts.day ?? "numeric",
      year: opts.year ?? "numeric",
      ...opts,
    }).format(new Date(date));
  } catch (_err) {
    return "";
  }
}
