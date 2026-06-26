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
