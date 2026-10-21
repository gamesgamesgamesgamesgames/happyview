import type { PublishableLexicon } from "@/types/lexicon-publishing";

import { unavailableSentence } from "./model";
import { TargetName } from "./target-name";

/** Where a lexicon publishes, and the rule or problem behind it. */
export function DestinationCell({ lexicon }: { lexicon: PublishableLexicon }) {
  const { target, publication } = lexicon;
  if (!target) {
    return <span className="text-muted-foreground text-sm">—</span>;
  }
  return (
    <div className="flex max-w-64 min-w-0 flex-col">
      <TargetName target={target} className="text-sm" />
      {lexicon.status === "target_unavailable" ? (
        <span className="text-muted-foreground text-xs text-pretty">
          {unavailableSentence(target)}
        </span>
      ) : lexicon.status === "moved" && publication?.did ? (
        <span className="text-muted-foreground truncate text-xs">
          Currently in <span className="font-mono">{publication.did}</span>
        </span>
      ) : (
        lexicon.rule_pattern && (
          <span className="text-muted-foreground truncate text-xs">
            via <span className="font-mono">{lexicon.rule_pattern}</span>
          </span>
        )
      )}
    </div>
  );
}
