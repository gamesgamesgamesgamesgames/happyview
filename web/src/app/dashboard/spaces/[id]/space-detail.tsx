"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { usePathname } from "next/navigation";
import Link from "next/link";
import { ChevronLeft, ChevronRight, ExternalLink } from "lucide-react";

import { useAccessGrant } from "@/hooks/use-access-grant";
import { useCurrentUser } from "@/hooks/use-current-user";
import { useConfig } from "@/lib/config-context";
import { InspectorDisabled } from "@/components/spaces/inspector-disabled";
import { AccountName } from "@/components/account-name";
import { blobCids } from "@/lib/blob-refs";
import { toastError } from "@/lib/format";
import {
  ApiError,
  SPACE_ACCESS_GRANT_REQUIRED,
  adminSpaceBlobUrl,
  getAdminSpace,
  getAdminSpaceRecords,
  getInspectorStatus,
} from "@/lib/api";
import type {
  AdminSpaceDetail,
  AdminSpaceRecord,
  InspectorStatus,
} from "@/types/spaces";
import { CodeBlock } from "@/components/code-block";
import { SiteHeader } from "@/components/site-header";
import { AccessGrantBanner } from "@/components/spaces/access-grant-banner";
import { AccessGrantDialog } from "@/components/spaces/access-grant-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "@/components/ui/sheet";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

const ALL = "__all__";
const PAGE_SIZE = 20;

const POLICY_LABELS: Record<string, string> = {
  publicPolicy: "Public",
  memberListPolicy: "Member list",
  managingAppPolicy: "Managing app",
  open: "Open",
  allowList: "Allow list",
};

function policyLabel(type: string): string {
  const name = type.split("#").pop() ?? type;
  return POLICY_LABELS[name] ?? name;
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-muted-foreground text-xs">{label}</span>
      <div className="text-sm">{children}</div>
    </div>
  );
}

function SpaceDetailContent() {
  const pathname = usePathname();
  const id = decodeURIComponent(
    pathname.split("/").filter(Boolean).pop() ?? "",
  );
  const { hasPermission } = useCurrentUser();
  const canInspect = hasPermission("spaces:inspect");
  const canManageSettings = hasPermission("settings:manage");

  const [detail, setDetail] = useState<AdminSpaceDetail | null>(null);
  const [inspector, setInspector] = useState<InspectorStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  const [collection, setCollection] = useState(ALL);
  const [repo, setRepo] = useState(ALL);
  const [records, setRecords] = useState<AdminSpaceRecord[]>([]);
  const [cursorStack, setCursorStack] = useState<string[]>([]);
  const [nextCursor, setNextCursor] = useState<string | undefined>();
  const [loading, setLoading] = useState(false);
  const [viewRecord, setViewRecord] = useState<AdminSpaceRecord | null>(null);
  const [requestOpen, setRequestOpen] = useState(false);

  useEffect(() => {
    getAdminSpace(id)
      .then(setDetail)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
    getInspectorStatus()
      .then(setInspector)
      .catch(() => setInspector(null));
  }, [id]);

  // Members and record authors: an account grant for any of them, including an
  // author who has since left, covers that account's records here.
  const accountDids = useMemo(
    () =>
      [
        ...new Set([
          ...(detail?.members.map((m) => m.did) ?? []),
          ...(detail?.authors ?? []),
        ]),
      ].sort(),
    [detail],
  );
  // Keyed on the loaded space too: member grants only match once that space's
  // members are known.
  const access = useAccessGrant(
    `${id}:${detail?.space.id ?? ""}`,
    (g) =>
      (g.scope === "space" && g.target === id) ||
      (g.scope === "account" && accountDids.includes(g.target)),
    Boolean(detail && inspector?.enabled && canInspect),
  );

  const fetchRecords = useCallback(
    async (filters: { collection: string; repo: string }, cursor?: string) => {
      setLoading(true);
      try {
        const data = await getAdminSpaceRecords(id, {
          collection: filters.collection === ALL ? undefined : filters.collection,
          repo: filters.repo === ALL ? undefined : filters.repo,
          limit: PAGE_SIZE,
          cursor,
        });
        setRecords(data.records);
        setNextCursor(data.cursor);
      } catch (e: unknown) {
        if (e instanceof ApiError && e.message === SPACE_ACCESS_GRANT_REQUIRED) {
          access.drop();
          return;
        }
        toastError("Failed to load records", e);
        setRecords([]);
        setNextCursor(undefined);
      } finally {
        setLoading(false);
      }
    },
    // access.drop is stable; the rest of `access` isn't needed here.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [id, access.drop],
  );

  useEffect(() => {
    if (!access.grant) {
      setRecords([]);
      setNextCursor(undefined);
      setCursorStack([]);
      setViewRecord(null);
      return;
    }
    const filters =
      access.grant.scope === "account"
        ? { collection, repo: access.grant.target }
        : { collection, repo };
    setRepo(filters.repo);
    fetchRecords(filters);
    // Reload only when the grant or the space changes; filter changes go
    // through applyFilters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [access.grant?.id, id]);

  function applyFilters(filters: { collection: string; repo: string }) {
    setCollection(filters.collection);
    setRepo(filters.repo);
    setCursorStack([]);
    fetchRecords(filters);
  }

  function handleNext() {
    if (!nextCursor) return;
    setCursorStack((prev) => [...prev, nextCursor]);
    fetchRecords({ collection, repo }, nextCursor);
  }

  function handlePrevious() {
    if (cursorStack.length === 0) return;
    const stack = cursorStack.slice(0, -1);
    setCursorStack(stack);
    fetchRecords({ collection, repo }, stack[stack.length - 1]);
  }

  if (error) {
    return (
      <>
        <SiteHeader title="Space" />
        <div className="p-4 md:p-6">
          <p className="text-destructive text-sm">{error}</p>
        </div>
      </>
    );
  }

  if (!detail) {
    return <SiteHeader title="Space" />;
  }

  const { space, members, collections } = detail;
  const blobs = viewRecord ? blobCids(viewRecord.record) : [];

  return (
    <>
      <SiteHeader title={space.display_name ?? space.skey} />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <Card>
          <CardHeader>
            <CardTitle>{space.display_name ?? space.skey}</CardTitle>
            {space.description && (
              <CardDescription>{space.description}</CardDescription>
            )}
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            <div className="sm:col-span-2">
              <Field label="URI">
                <span className="font-mono text-xs break-all">{space.uri}</span>
              </Field>
            </div>
            <Field label="Type">
              <span className="font-mono text-xs">{space.type}</span>
            </Field>
            <Field label="Creator">
              <AccountName did={space.creator_did} />
            </Field>
            <Field label="Authority">
              <AccountName did={space.authority_did} />
            </Field>
            <Field label="Created">
              {new Date(space.created_at).toLocaleString()}
            </Field>
            <Field label="Read policy">
              <Badge variant="outline">{policyLabel(space.read_policy.$type)}</Badge>
            </Field>
            <Field label="Write policy">
              <Badge variant="outline">
                {policyLabel(space.write_policy.$type)}
              </Badge>
            </Field>
            <Field label="App access">
              <Badge variant="outline">{policyLabel(space.app_access.$type)}</Badge>
            </Field>
          </CardContent>
        </Card>

        <div className="grid gap-4 lg:grid-cols-2">
          <Card>
            <CardHeader>
              <CardTitle>Members</CardTitle>
              <CardDescription>
                Includes members added through delegated spaces.
              </CardDescription>
            </CardHeader>
            <CardContent>
              {members.length === 0 ? (
                <p className="text-muted-foreground text-sm">No members.</p>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>DID</TableHead>
                      <TableHead>Access</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {members.map((member) => (
                      <TableRow key={member.did}>
                        <TableCell>
                          <Link
                            href={`/dashboard/spaces/account/?did=${encodeURIComponent(member.did)}`}
                            className="underline underline-offset-2"
                          >
                            <AccountName did={member.did} />
                          </Link>
                        </TableCell>
                        <TableCell className="flex gap-1">
                          {member.read && <Badge variant="outline">read</Badge>}
                          {member.write && <Badge variant="outline">write</Badge>}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Collections</CardTitle>
            </CardHeader>
            <CardContent>
              {collections.length === 0 ? (
                <p className="text-muted-foreground text-sm">No records.</p>
              ) : (
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Collection</TableHead>
                      <TableHead className="text-right">Records</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {collections.map((c) => (
                      <TableRow key={c.collection}>
                        <TableCell className="font-mono text-xs">
                          {c.collection}
                        </TableCell>
                        <TableCell className="text-right tabular-nums">
                          {c.count}
                        </TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )}
            </CardContent>
          </Card>
        </div>

        {canInspect && (
          <Card>
            <CardHeader>
              <CardTitle>Records</CardTitle>
              {!inspector?.enabled ? (
                <>
                  <CardDescription>
                    The space inspector is turned off on this instance.
                  </CardDescription>
                  {canManageSettings && (
                    <CardAction>
                      <Button variant="outline" asChild>
                        <Link href="/dashboard/settings/general/">
                          Open settings
                        </Link>
                      </Button>
                    </CardAction>
                  )}
                </>
              ) : !access.grant ? (
                <>
                  <CardDescription>
                    This space is private to its members. To read its
                    records, request access and give a reason. Access
                    expires, and the reason and every read are logged.
                  </CardDescription>
                  <CardAction>
                    <Button variant="outline" onClick={() => setRequestOpen(true)}>
                      Request access
                    </Button>
                  </CardAction>
                </>
              ) : null}
            </CardHeader>
            {inspector?.enabled && access.grant && (
              <CardContent className="flex flex-col gap-4">
                <AccessGrantBanner
                  grant={access.grant}
                  remainingMs={access.remainingMs}
                  onEnd={() =>
                    access.end().catch((e) => toastError("Couldn't end access", e))
                  }
                />

                <div className="flex flex-wrap items-center gap-2">
                  <Select
                    value={collection}
                    onValueChange={(value) =>
                      applyFilters({ collection: value, repo })
                    }
                  >
                    <SelectTrigger className="h-8 w-72 text-sm" aria-label="Collection">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={ALL}>All collections</SelectItem>
                      {collections.map((c) => (
                        <SelectItem key={c.collection} value={c.collection}>
                          {c.collection}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <Select
                    value={repo}
                    disabled={access.grant.scope === "account"}
                    onValueChange={(value) =>
                      applyFilters({ collection, repo: value })
                    }
                  >
                    <SelectTrigger className="h-8 w-72 text-sm" aria-label="Author">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value={ALL}>All authors</SelectItem>
                      {accountDids.map((did) => (
                        <SelectItem key={did} value={did}>
                          <AccountName did={did} />
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                </div>

                {records.length === 0 ? (
                  <p className="text-muted-foreground text-sm">
                    {loading ? "Loading…" : "No records match."}
                  </p>
                ) : (
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Author</TableHead>
                        <TableHead>Collection</TableHead>
                        <TableHead>Record key</TableHead>
                        <TableHead>Indexed</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {records.map((record) => (
                        <TableRow
                          key={record.uri}
                          className="cursor-pointer"
                          onClick={() => setViewRecord(record)}
                        >
                          <TableCell className="whitespace-nowrap">
                            <AccountName did={record.did} />
                          </TableCell>
                          <TableCell className="font-mono text-xs">
                            {record.collection}
                          </TableCell>
                          <TableCell className="font-mono text-xs">
                            <button
                              type="button"
                              className="underline-offset-2 hover:underline focus-visible:underline"
                              onClick={(e) => {
                                e.stopPropagation();
                                setViewRecord(record);
                              }}
                            >
                              {record.rkey}
                            </button>
                          </TableCell>
                          <TableCell className="text-xs whitespace-nowrap">
                            {new Date(record.indexed_at).toLocaleString()}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                )}

                <div className="flex items-center justify-end gap-2">
                  <Button
                    aria-label="Go to previous page"
                    title="Previous page"
                    variant="outline"
                    size="icon"
                    className="size-8"
                    disabled={cursorStack.length === 0 || loading}
                    onClick={handlePrevious}
                  >
                    <ChevronLeft />
                  </Button>
                  <Button
                    aria-label="Go to next page"
                    title="Next page"
                    variant="outline"
                    size="icon"
                    className="size-8"
                    disabled={!nextCursor || loading}
                    onClick={handleNext}
                  >
                    <ChevronRight />
                  </Button>
                </div>
              </CardContent>
            )}
          </Card>
        )}

        <Sheet
          open={viewRecord != null}
          onOpenChange={(open) => {
            if (!open) setViewRecord(null);
          }}
        >
          <SheetContent className="flex flex-col overflow-hidden">
            {viewRecord && (
              <>
                <SheetHeader>
                  <SheetTitle className="sr-only">Record detail</SheetTitle>
                </SheetHeader>
                <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-4 pb-4">
                  <div className="grid grid-cols-2 gap-4">
                    <div className="col-span-2">
                      <Field label="URI">
                        <span className="font-mono text-xs break-all">
                          {viewRecord.uri}
                        </span>
                      </Field>
                    </div>
                    <Field label="Author">
                      <AccountName did={viewRecord.did} />
                    </Field>
                    <Field label="Collection">
                      <span className="font-mono text-xs">
                        {viewRecord.collection}
                      </span>
                    </Field>
                    <Field label="CID">
                      <span className="font-mono text-xs break-all">
                        {viewRecord.cid}
                      </span>
                    </Field>
                    <Field label="Indexed">
                      {new Date(viewRecord.indexed_at).toLocaleString()}
                    </Field>
                  </div>

                  {blobs.length > 0 && (
                    <Field label="Blobs">
                      <ul className="flex flex-col gap-1">
                        {blobs.map((cid) => (
                          <li key={cid}>
                            <a
                              href={adminSpaceBlobUrl(id, cid)}
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
                    </Field>
                  )}

                  <Field label="Record">
                    <CodeBlock code={JSON.stringify(viewRecord.record, null, 2)} />
                  </Field>
                </div>
              </>
            )}
          </SheetContent>
        </Sheet>

        {inspector && detail && (
          <AccessGrantDialog
            open={requestOpen}
            onOpenChange={setRequestOpen}
            maxMinutes={inspector.max_grant_minutes}
            defaultMinutes={inspector.default_grant_minutes}
            scopeOptions={[
              { scope: "space", target: id, label: "This space: every record and blob, from every author" },
              ...accountDids.map((did) => ({
                scope: "account" as const,
                target: did,
                label: (
                  <>
                    One account, in every space: <AccountName did={did} />
                  </>
                ),
              })),
            ]}
            onGranted={(g) => access.setGrant(g)}
          />
        )}
      </div>
    </>
  );
}

export default function SpaceDetail() {
  const { features } = useConfig();
  if (!features.space_inspector) return <InspectorDisabled title="Space" />;
  return <SpaceDetailContent />;
}
