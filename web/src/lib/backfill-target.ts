import type { BackfillJob } from "@/types/backfill"

export function backfillTargetLabel(
  job: Pick<BackfillJob, "scope" | "did" | "total_repos">,
): string {
  if (job.scope !== "dids") return "All"
  if (job.did) return job.did
  const count = job.total_repos ?? 0
  return `${count} ${count === 1 ? "account" : "accounts"}`
}
