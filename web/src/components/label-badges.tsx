import { Badge } from "@/components/ui/badge";
import type { RecordLabel } from "@/types/records";

const CONTENT_WARNING_LABELS = new Set([
  "nudity",
  "sexual",
  "graphic-media",
  "violence",
  "gore",
]);

const MODERATION_LABELS = new Set(["spam", "impersonation"]);

function getLabelVariant(
  val: string,
  isSelfLabel: boolean,
): "destructive" | "outline" | "secondary" {
  if (isSelfLabel) return "outline";
  if (CONTENT_WARNING_LABELS.has(val)) return "destructive";
  return "secondary";
}

function getLabelClassName(val: string, isSelfLabel: boolean): string {
  if (isSelfLabel) return "";
  if (MODERATION_LABELS.has(val))
    return "bg-amber-500 text-white border-amber-500";
  return "";
}

interface LabelBadgesProps {
  labels: RecordLabel[];
  recordDid: string;
}

export function LabelBadges({ labels, recordDid }: LabelBadgesProps) {
  if (labels.length === 0) return null;

  // Group labels by source.
  const grouped = new Map<string, RecordLabel[]>();
  for (const label of labels) {
    const existing = grouped.get(label.src) ?? [];
    existing.push(label);
    grouped.set(label.src, existing);
  }

  return (
    <div className="flex flex-wrap gap-1">
      {Array.from(grouped.entries()).map(([src, srcLabels], groupIdx) => (
        <div key={src} className="flex items-center gap-1">
          {groupIdx > 0 && (
            <span className="text-muted-foreground text-xs mx-0.5">·</span>
          )}
          {srcLabels.map((label) => {
            const isSelfLabel = label.src === recordDid;
            return (
              <Badge
                key={`${label.src}-${label.val}`}
                variant={getLabelVariant(label.val, isSelfLabel)}
                className={getLabelClassName(label.val, isSelfLabel)}
                title={`Source: ${label.src}`}
              >
                {label.val}
              </Badge>
            );
          })}
        </div>
      ))}
    </div>
  );
}
