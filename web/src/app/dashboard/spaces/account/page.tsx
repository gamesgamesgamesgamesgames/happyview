"use client";

import { Suspense, useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { ChevronLeft, ChevronRight, ExternalLink } from "lucide-react";

import { useAccessGrant } from "@/hooks/use-access-grant";
import { useCurrentUser } from "@/hooks/use-current-user";
import { useConfig } from "@/lib/config-context";
import { InspectorDisabled } from "@/components/spaces/inspector-disabled";
import { blobCids } from "@/lib/blob-refs";
import { toastError } from "@/lib/format";
import {
  ApiError,
  SPACE_ACCESS_GRANT_REQUIRED,
  adminSpaceBlobUrl,
  getAccountSpaceRecords,
  getAccountSpaces,
  getInspectorStatus,
} from "@/lib/api";
import type { AdminAccountRecord, AdminAccountSpace, InspectorStatus } from "@/types/spaces";
import { AccessGrantBanner } from "@/components/spaces/access-grant-banner";
import { AccessGrantDialog } from "@/components/spaces/access-grant-dialog";
import { CodeBlock } from "@/components/code-block";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const PAGE_SIZE = 20;

function AccountView() {
  const did = useSearchParams().get("did") ?? "";
  const { hasPermission } = useCurrentUser();
  const canInspect = hasPermission("spaces:inspect");

  const [spaces, setSpaces] = useState<AdminAccountSpace[] | null>(null);
  const [inspector, setInspector] = useState<InspectorStatus | null>(null);
  const [records, setRecords] = useState<AdminAccountRecord[]>([]);
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [loading, setLoading] = useState(false);
  const [requestOpen, setRequestOpen] = useState(false);
  const [viewRecord, setViewRecord] = useState<AdminAccountRecord | null>(null);

  const access = useAccessGrant(
    (g) => g.scope === "account" && g.target === did,
    Boolean(inspector?.enabled && canInspect && did),
  );

  useEffect(() => {
    if (!did) return;
    getAccountSpaces(did)
      .then((r) => setSpaces(r.spaces))
      .catch((e) => toastError("Failed to load spaces", e));
    getInspectorStatus().then(setInspector).catch(() => setInspector(null));
  }, [did]);

  const fetchRecords = useCallback(
    async (cursor?: string) => {
      setLoading(true);
      try {
        const data = await getAccountSpaceRecords(did, { limit: PAGE_SIZE, cursor });
        setRecords(data.records);
        setNextCursor(data.cursor);
      } catch (e: unknown) {
        if (e instanceof ApiError && e.message === SPACE_ACCESS_GRANT_REQUIRED) {
          access.drop();
          return;
        }
        toastError("Failed to load records", e);
      } finally {
        setLoading(false);
      }
    },
    // access.drop is stable; the rest of `access` isn't needed here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [did, access.drop],
  );

  useEffect(() => {
    setCursorStack([]);
    setViewRecord(null);
    if (access.grant) fetchRecords();
    else setRecords([]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [access.grant?.id]);

  if (!did) {
    return (
      <>
        <SiteHeader title="Account" />
        <p className="text-muted-foreground p-4 text-sm md:p-6">No account selected.</p>
      </>
    );
  }

  const blobs = viewRecord ? blobCids(viewRecord.record) : [];

  return (
    <>
      <SiteHeader title="Account" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <Card>
          <CardHeader>
            <CardTitle className="font-mono text-sm break-all">{did}</CardTitle>
            <CardDescription>Spaces this account belongs to or has written in.</CardDescription>
          </CardHeader>
          <CardContent>
            {spaces === null ? null : spaces.length === 0 ? (
              <p className="text-muted-foreground text-sm">No spaces.</p>
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Space</TableHead>
                    <TableHead className="text-right">Records</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {spaces.map(({ space, record_count }) => (
                    <TableRow key={space.id}>
                      <TableCell className="font-mono text-xs break-all">
                        <Link href={`/dashboard/spaces/${encodeURIComponent(space.id)}/`} className="underline underline-offset-2">
                          {space.uri}
                        </Link>
                      </TableCell>
                      <TableCell className="text-right tabular-nums">{record_count}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </CardContent>
        </Card>

        {canInspect && (
          <Card>
            <CardHeader>
              <CardTitle>Records</CardTitle>
              <CardDescription>
                {!inspector?.enabled
                  ? "The space inspector is turned off on this instance."
                  : access.grant
                    ? "Records this account wrote in any space, newest first."
                    : "To read this account's records across spaces, request access and give a reason. Access expires, and the reason and every read are logged."}
              </CardDescription>
              {inspector?.enabled && !access.grant && (
                <CardAction>
                  <Button variant="outline" onClick={() => setRequestOpen(true)}>
                    Request access
                  </Button>
                </CardAction>
              )}
            </CardHeader>
            {access.grant && (
              <CardContent className="flex flex-col gap-4">
                <AccessGrantBanner
                  grant={access.grant}
                  remainingMs={access.remainingMs}
                  onEnd={() => access.end().catch((e) => toastError("Couldn't end access", e))}
                />
                {records.length === 0 ? (
                  <p className="text-muted-foreground text-sm">{loading ? "Loading…" : "No records."}</p>
                ) : (
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Space</TableHead>
                        <TableHead>Collection</TableHead>
                        <TableHead>Record key</TableHead>
                        <TableHead>Indexed</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {records.map((r) => (
                        <TableRow key={r.uri} className="cursor-pointer" onClick={() => setViewRecord(r)}>
                          <TableCell className="font-mono text-xs break-all">{r.space_uri}</TableCell>
                          <TableCell className="font-mono text-xs">{r.collection}</TableCell>
                          <TableCell className="font-mono text-xs">{r.rkey}</TableCell>
                          <TableCell className="text-xs whitespace-nowrap">
                            {new Date(r.indexed_at).toLocaleString()}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                )}
                <div className="flex items-center justify-end gap-2">
                  <Button
                    aria-label="Go to previous page"
                    variant="outline"
                    size="icon"
                    className="size-8"
                    disabled={cursorStack.length === 0 || loading}
                    onClick={() => {
                      const stack = cursorStack.slice(0, -1);
                      setCursorStack(stack);
                      fetchRecords(stack[stack.length - 1]);
                    }}
                  >
                    <ChevronLeft />
                  </Button>
                  <Button
                    aria-label="Go to next page"
                    variant="outline"
                    size="icon"
                    className="size-8"
                    disabled={!nextCursor || loading}
                    onClick={() => {
                      if (!nextCursor) return;
                      setCursorStack((prev) => [...prev, nextCursor]);
                      fetchRecords(nextCursor);
                    }}
                  >
                    <ChevronRight />
                  </Button>
                </div>
              </CardContent>
            )}
          </Card>
        )}

        {inspector && (
          <AccessGrantDialog
            open={requestOpen}
            onOpenChange={setRequestOpen}
            maxMinutes={inspector.max_grant_minutes}
            defaultMinutes={inspector.default_grant_minutes}
            scopeOptions={[{ scope: "account", target: did, label: `This account: ${did}` }]}
            onGranted={(g) => access.setGrant(g)}
          />
        )}

        <Sheet open={viewRecord != null} onOpenChange={(open) => !open && setViewRecord(null)}>
          <SheetContent className="flex flex-col overflow-hidden">
            {viewRecord && (
              <>
                <SheetHeader>
                  <SheetTitle className="sr-only">Record detail</SheetTitle>
                </SheetHeader>
                <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4">
                  <p className="font-mono text-xs break-all">{viewRecord.uri}</p>
                  {blobs.length > 0 && (
                    <ul className="flex flex-col gap-1">
                      {blobs.map((cid) => (
                        <li key={cid}>
                          <a
                            href={adminSpaceBlobUrl(viewRecord.space_id, cid)}
                            target="_blank"
                            rel="noopener noreferrer"
                            className="inline-flex items-center gap-1 font-mono text-xs underline underline-offset-2"
                          >
                            {cid}
                            <ExternalLink className="size-3" aria-hidden />
                          </a>
                        </li>
                      ))}
                    </ul>
                  )}
                  <CodeBlock code={JSON.stringify(viewRecord.record, null, 2)} />
                </div>
              </>
            )}
          </SheetContent>
        </Sheet>
      </div>
    </>
  );
}

export default function AccountPage() {
  const { features } = useConfig();
  if (!features.space_inspector) return <InspectorDisabled title="Account" />;
  return (
    <Suspense>
      <AccountView />
    </Suspense>
  );
}
