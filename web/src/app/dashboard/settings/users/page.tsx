"use client";

import React, { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronRight, Search, Shield, Trash2 } from "lucide-react";
import { toast } from "sonner";

import { useAuth } from "@/lib/auth-context";
import { toastError } from "@/lib/format";
import {
  getUsers,
  addUser,
  deleteUser,
  updateUserPermissions,
  transferSuper,
  getPermissions,
} from "@/lib/api";
import type { PermissionEntry, PermissionTemplate } from "@/lib/api";
import type { UserSummary } from "@/types/users";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from "@/components/ui/alert-dialog";
import { SiteHeader } from "@/components/site-header";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import {
  ResponsiveDialog,
  ResponsiveDialogClose,
  ResponsiveDialogContent,
  ResponsiveDialogDescription,
  ResponsiveDialogFooter,
  ResponsiveDialogHeader,
  ResponsiveDialogTitle,
  ResponsiveDialogTrigger,
} from "@/components/ui/responsive-dialog";
import {
  Sheet,
  SheetContent,
  SheetFooter,
  SheetHeader,
  SheetTitle,
  SheetDescription,
} from "@/components/ui/sheet";

type BskyProfile = {
  avatar?: string;
  displayName?: string;
  description?: string;
};

function buildCategories(
  permissions: PermissionEntry[],
): Record<string, PermissionEntry[]> {
  const cats: Record<string, PermissionEntry[]> = {};
  for (const p of permissions) {
    if (!cats[p.category]) cats[p.category] = [];
    cats[p.category].push(p);
  }
  return cats;
}

export default function UsersPage() {
  const { did: currentDid } = useAuth();
  const [users, setUsers] = useState<UserSummary[]>([]);
  const [handles, setHandles] = useState<Record<string, string>>({});
  const [selectedUserId, setSelectedUserId] = useState<string | null>(null);
  const [pendingPermissions, setPendingPermissions] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [permSearch, setPermSearch] = useState("");
  const [permissionEntries, setPermissionEntries] = useState<PermissionEntry[]>(
    [],
  );
  const [profiles, setProfiles] = useState<Record<string, BskyProfile>>({});
  const [templates, setTemplates] = useState<PermissionTemplate[]>([]);

  const permissionCategories = React.useMemo(
    () => buildCategories(permissionEntries),
    [permissionEntries],
  );
  const filteredCategories = useMemo(() => {
    if (!permSearch.trim()) return permissionCategories;
    const terms = permSearch.toLowerCase().split(/\s+/);
    const result: Record<string, PermissionEntry[]> = {};
    for (const [category, permissions] of Object.entries(
      permissionCategories,
    )) {
      const matched = permissions.filter((p) => {
        const haystack =
          `${p.name} ${p.description} ${p.category} ${p.key}`.toLowerCase();
        return terms.every((term) => haystack.includes(term));
      });
      if (matched.length > 0) result[category] = matched;
    }
    return result;
  }, [permissionCategories, permSearch]);
  const allPermissionKeys = React.useMemo(
    () => permissionEntries.map((p) => p.key),
    [permissionEntries],
  );
  const templatePermissions = React.useMemo(() => {
    const map: Record<string, string[]> = {};
    for (const t of templates) map[t.key] = t.permissions;
    return map;
  }, [templates]);

  const currentUser = users.find((u) => u.did === currentDid);
  const isCurrentUserSuper = currentUser?.is_super ?? false;

  const load = useCallback(() => {
    getUsers()
      .then(setUsers)
      .catch((e) => toastError("Failed to load users", e));
  }, []);

  useEffect(() => {
    load();
    getPermissions()
      .then((catalog) => {
        setPermissionEntries(catalog.permissions);
        setTemplates(catalog.templates);
      })
      .catch((e) => toastError("Failed to load permissions", e));
  }, [load]);

  // Resolve DIDs to handles via PLC directory
  useEffect(() => {
    const newDids = users.map((u) => u.did).filter((did) => !(did in handles));
    if (newDids.length === 0) return;
    for (const did of newDids) {
      fetch(`https://plc.directory/${encodeURIComponent(did)}`)
        .then((res) => (res.ok ? res.json() : null))
        .then((data) => {
          if (!data) return;
          const handle = data.alsoKnownAs
            ?.find((aka: string) => aka.startsWith("at://"))
            ?.replace("at://", "");
          if (handle) {
            setHandles((prev) => ({ ...prev, [did]: handle }));
          }
        })
        .catch(() => {});
    }
  }, [users, handles]);

  // Initialize pending permissions when a user is selected
  useEffect(() => {
    if (!selectedUserId) return;
    const user = users.find((u) => u.id === selectedUserId);
    if (user) setPendingPermissions([...user.permissions]);
  }, [selectedUserId, users]);

  // Fetch Bluesky profile when a user is selected
  useEffect(() => {
    if (!selectedUserId) return;
    const user = users.find((u) => u.id === selectedUserId);
    if (!user || user.did in profiles) return;
    fetch(
      `https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor=${encodeURIComponent(user.did)}`,
    )
      .then((res) => (res.ok ? res.json() : null))
      .then((data) => {
        if (!data) return;
        setProfiles((prev) => ({
          ...prev,
          [user.did]: {
            avatar: data.avatar,
            displayName: data.displayName,
            description: data.description,
          },
        }));
      })
      .catch(() => {});
  }, [selectedUserId, users, profiles]);

  async function handleDelete(id: string) {
    try {
      await deleteUser(id);
      toast.success("User deleted");
      load();
    } catch (e: unknown) {
      toastError("Failed to delete user", e);
    }
  }

  function handleTogglePermission(
    _user: UserSummary,
    permission: string,
    enabled: boolean,
  ) {
    setPendingPermissions((prev) => {
      const perms = new Set(prev);
      const [ns, action] = permission.split(":");

      const nsReadPerm = allPermissionKeys.find(
        (k) =>
          k.startsWith(`${ns}:`) &&
          (k.endsWith(":read") || k.endsWith(":view")),
      );
      const isReadAction = action === "read" || action === "view";

      if (enabled) {
        perms.add(permission);
        if (!isReadAction && nsReadPerm) perms.add(nsReadPerm);
        if (permission === "records:delete-collection")
          perms.add("records:delete");
      } else {
        perms.delete(permission);
        if (isReadAction) {
          for (const p of prev) {
            if (p.startsWith(`${ns}:`) && p !== permission) perms.delete(p);
          }
        }
        if (permission === "records:delete")
          perms.delete("records:delete-collection");
      }

      return [...perms];
    });
  }

  async function handleSavePermissions(
    userId: string,
    originalPermissions: string[],
  ) {
    const originalSet = new Set(originalPermissions);
    const pendingSet = new Set(pendingPermissions);

    const grant = pendingPermissions.filter((p) => !originalSet.has(p));
    const revoke = originalPermissions.filter((p) => !pendingSet.has(p));

    if (grant.length === 0 && revoke.length === 0) return;

    setSaving(true);
    try {
      const body: { grant?: string[]; revoke?: string[] } = {};
      if (grant.length > 0) body.grant = grant;
      if (revoke.length > 0) body.revoke = revoke;
      await updateUserPermissions(userId, body);
      toast.success("Permissions updated");
      load();
    } catch (e: unknown) {
      toastError("Failed to save permissions", e);
    } finally {
      setSaving(false);
    }
  }

  async function handleTransferSuper(targetUserId: string) {
    try {
      await transferSuper({ target_user_id: targetUserId });
      toast.success("Ownership transferred");
      load();
    } catch (e: unknown) {
      toastError("Failed to transfer ownership", e);
    }
  }

  return (
    <>
      <SiteHeader title="Users" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">Users</h2>
          {(isCurrentUserSuper ||
            currentUser?.permissions.includes("users:create")) && (
            <AddUserDialog
              onSuccess={load}
              templates={templates}
              templatePermissions={templatePermissions}
            />
          )}
        </div>

        <div className="overflow-clip rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>User</TableHead>
                <TableHead>Permissions</TableHead>
                <TableHead>Created</TableHead>
                <TableHead>Last Used</TableHead>
                <TableHead className="w-8" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {users.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={5}
                    className="text-muted-foreground text-center"
                  >
                    No users yet. The first authenticated user is automatically granted owner permissions.
                  </TableCell>
                </TableRow>
              )}
              {users.map((user) => (
                <TableRow
                  key={user.id}
                  className="cursor-pointer hover:bg-muted/50"
                  onClick={() => setSelectedUserId(user.id)}
                >
                  <TableCell className="text-sm">
                    <div className="flex items-center gap-2">
                      <div className="flex flex-col">
                        {handles[user.did] && (
                          <span className="font-medium">
                            @{handles[user.did]}
                          </span>
                        )}
                        <span className="font-mono text-muted-foreground text-xs">
                          {user.did}
                        </span>
                      </div>
                      {user.is_super && (
                        <Badge variant="secondary" className="text-xs">
                          Owner
                        </Badge>
                      )}
                    </div>
                  </TableCell>
                  <TableCell>
                    {user.is_super
                      ? `${allPermissionKeys.length}/${allPermissionKeys.length}`
                      : `${user.permissions.filter((p) => allPermissionKeys.includes(p)).length}/${allPermissionKeys.length}`}
                  </TableCell>
                  <TableCell>
                    {new Date(user.created_at).toLocaleString()}
                  </TableCell>
                  <TableCell>
                    {user.last_used_at
                      ? new Date(user.last_used_at).toLocaleString()
                      : "Never"}
                  </TableCell>
                  <TableCell className="w-8 pr-2">
                    <ChevronRight className="size-4 text-muted-foreground" />
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>

        {(() => {
          const selectedUser = users.find((u) => u.id === selectedUserId);
          return (
            <Sheet
              modal={false}
              open={!!selectedUser}
              onOpenChange={(open) => {
                if (!open) {
                  const user = users.find((u) => u.id === selectedUserId);
                  if (user) {
                    const origSet = new Set(user.permissions);
                    const pendSet = new Set(pendingPermissions);
                    const unsaved =
                      pendingPermissions.some((p) => !origSet.has(p)) ||
                      user.permissions.some((p) => !pendSet.has(p));
                    if (unsaved) {
                      toast.warning(
                        "You have unsaved changes. Save or cancel before closing.",
                      );
                      return;
                    }
                  }
                  setSelectedUserId(null);
                  setPermSearch("");
                }
              }}
            >
              <SheetContent
                className="overflow-hidden"
                onInteractOutside={(e) => {
                  if (
                    e.target instanceof HTMLElement &&
                    e.target.closest("[data-sonner-toaster]")
                  ) {
                    e.preventDefault();
                  }
                }}
              >
                {selectedUser && (
                  <>
                    <SheetHeader>
                      <SheetTitle>User</SheetTitle>
                    </SheetHeader>

                    <Card className="mx-4">
                      <CardContent className="flex items-start gap-3">
                        {profiles[selectedUser.did]?.avatar && (
                          <img
                            src={profiles[selectedUser.did].avatar}
                            alt=""
                            className="size-12 rounded-full shrink-0"
                          />
                        )}
                        <div className="flex flex-col gap-0.5 min-w-0">
                          <p className="font-semibold">
                            {profiles[selectedUser.did]?.displayName ||
                            handles[selectedUser.did] ? (
                              <>
                                {profiles[selectedUser.did]?.displayName && (
                                  <span>
                                    {profiles[selectedUser.did].displayName}
                                  </span>
                                )}
                                {handles[selectedUser.did] && (
                                  <span className="text-muted-foreground font-normal text-sm ml-1">
                                    @{handles[selectedUser.did]}
                                  </span>
                                )}
                              </>
                            ) : (
                              <span className="font-mono text-sm">
                                {selectedUser.did}
                              </span>
                            )}
                          </p>
                          <p className="font-mono text-xs text-muted-foreground break-all">
                            {selectedUser.did}
                          </p>
                          {profiles[selectedUser.did]?.description && (
                            <p className="mt-1 text-xs text-muted-foreground line-clamp-2">
                              {profiles[selectedUser.did].description}
                            </p>
                          )}
                        </div>
                      </CardContent>
                    </Card>

                    {(() => {
                      const originalSet = new Set(selectedUser.permissions);
                      const pendingSet = new Set(pendingPermissions);
                      const added = pendingPermissions.filter(
                        (p) => !originalSet.has(p),
                      ).length;
                      const removed = selectedUser.permissions.filter(
                        (p) => !pendingSet.has(p),
                      ).length;
                      const hasChanges = added > 0 || removed > 0;
                      return (
                        <>
                          <div className="grid grid-cols-2 gap-4 text-sm px-4">
                            <div>
                              <span className="text-muted-foreground text-xs">
                                Role
                              </span>
                              <p className="text-xs">
                                {selectedUser.is_super ? (
                                  <Badge
                                    variant="secondary"
                                    className="text-xs"
                                  >
                                    Owner
                                  </Badge>
                                ) : (
                                  "Member"
                                )}
                              </p>
                            </div>
                            <div>
                              <span className="text-muted-foreground text-xs">
                                Permissions
                              </span>
                              <p className="text-xs tabular-nums">
                                {selectedUser.is_super
                                  ? `${allPermissionKeys.length}/${allPermissionKeys.length}`
                                  : `${pendingPermissions.filter((p) => allPermissionKeys.includes(p)).length}/${allPermissionKeys.length}`}
                                {hasChanges && (
                                  <span className="ml-1.5">
                                    {added > 0 && (
                                      <span className="text-green-500">
                                        +{added}
                                      </span>
                                    )}
                                    {added > 0 && removed > 0 && " "}
                                    {removed > 0 && (
                                      <span className="text-red-500">
                                        -{removed}
                                      </span>
                                    )}
                                  </span>
                                )}
                              </p>
                            </div>
                            <div>
                              <span className="text-muted-foreground text-xs">
                                Created
                              </span>
                              <p className="text-xs">
                                {new Date(
                                  selectedUser.created_at,
                                ).toLocaleString()}
                              </p>
                            </div>
                            <div>
                              <span className="text-muted-foreground text-xs">
                                Last Active
                              </span>
                              <p className="text-xs">
                                {selectedUser.last_used_at
                                  ? new Date(
                                      selectedUser.last_used_at,
                                    ).toLocaleString()
                                  : "Never"}
                              </p>
                            </div>
                          </div>

                          <hr className="mx-4" />

                          <div className="relative px-4">
                            <Search className="absolute left-6.5 top-2.5 size-4 text-muted-foreground" />
                            <Input
                              placeholder="Search permissions..."
                              value={permSearch}
                              onChange={(e) => setPermSearch(e.target.value)}
                              className="pl-9"
                            />
                          </div>

                          <div className="flex-1 min-h-0 overflow-y-auto px-4 pb-4">
                            <PermissionsPanel
                              user={selectedUser}
                              isSelf={selectedUser.did === currentDid}
                              currentUserPermissions={
                                currentUser?.permissions ?? []
                              }
                              isCurrentUserSuper={isCurrentUserSuper}
                              filteredCategories={filteredCategories}
                              pendingPermissions={pendingPermissions}
                              originalPermissions={selectedUser.permissions}
                              onToggle={handleTogglePermission}
                            />
                          </div>

                          <SheetFooter className="border-t flex-row">
                            <div className="flex items-center gap-2">
                              <AlertDialog>
                                <AlertDialogTrigger asChild>
                                  <Button
                                    variant="destructive"
                                    size="sm"
                                    disabled={
                                      selectedUser.is_super ||
                                      selectedUser.did === currentDid ||
                                      (!isCurrentUserSuper &&
                                        !currentUser?.permissions.includes(
                                          "users:delete",
                                        ))
                                    }
                                  >
                                    <Trash2 className="mr-1 size-3.5" />
                                    Delete User
                                  </Button>
                                </AlertDialogTrigger>
                                <AlertDialogContent>
                                  <AlertDialogHeader>
                                    <AlertDialogTitle>Delete user?</AlertDialogTitle>
                                    <AlertDialogDescription>
                                      This will permanently remove the user and revoke all their permissions. This action cannot be undone.
                                    </AlertDialogDescription>
                                  </AlertDialogHeader>
                                  <AlertDialogFooter>
                                    <AlertDialogCancel>Cancel</AlertDialogCancel>
                                    <AlertDialogAction variant="destructive" onClick={() => { handleDelete(selectedUser.id); setSelectedUserId(null); }}>Delete</AlertDialogAction>
                                  </AlertDialogFooter>
                                </AlertDialogContent>
                              </AlertDialog>
                              {isCurrentUserSuper && (
                                <TransferOwnershipDialog
                                  user={selectedUser}
                                  disabled={selectedUser.did === currentDid}
                                  onConfirm={() =>
                                    handleTransferSuper(selectedUser.id)
                                  }
                                />
                              )}
                            </div>
                            <div className="ml-auto flex items-center gap-2">
                              <Button
                                variant="outline"
                                size="sm"
                                disabled={!hasChanges || saving}
                                onClick={() =>
                                  setPendingPermissions([
                                    ...selectedUser.permissions,
                                  ])
                                }
                              >
                                Cancel
                              </Button>
                              <Button
                                size="sm"
                                disabled={
                                  !hasChanges ||
                                  saving ||
                                  selectedUser.is_super ||
                                  selectedUser.did === currentDid ||
                                  (!isCurrentUserSuper &&
                                    !currentUser?.permissions.includes(
                                      "users:update",
                                    ))
                                }
                                onClick={() =>
                                  handleSavePermissions(
                                    selectedUser.id,
                                    selectedUser.permissions,
                                  )
                                }
                              >
                                {saving ? "Saving..." : "Save"}
                              </Button>
                            </div>
                          </SheetFooter>
                        </>
                      );
                    })()}
                  </>
                )}
              </SheetContent>
            </Sheet>
          );
        })()}
      </div>
    </>
  );
}

function PermissionsPanel({
  user,
  isSelf,
  currentUserPermissions,
  isCurrentUserSuper,
  filteredCategories,
  pendingPermissions,
  originalPermissions,
  onToggle,
}: {
  user: UserSummary;
  isSelf: boolean;
  currentUserPermissions: string[];
  isCurrentUserSuper: boolean;
  filteredCategories: Record<string, PermissionEntry[]>;
  pendingPermissions: string[];
  originalPermissions: string[];
  onToggle: (user: UserSummary, permission: string, enabled: boolean) => void;
}) {
  const canUpdate =
    isCurrentUserSuper || currentUserPermissions.includes("users:update");
  const originalSet = new Set(originalPermissions);

  return (
    <div className="grid gap-8 lg:grid-cols-2">
      {Object.entries(filteredCategories).map(([category, permissions]) => (
        <div key={category} className="flex flex-col items-start gap-2">
          <p className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            {category}
          </p>
          <div className="flex flex-col gap-3 w-full">
            {permissions.map((perm) => {
              const enabled =
                user.is_super || pendingPermissions.includes(perm.key);
              const wasEnabled = user.is_super || originalSet.has(perm.key);
              const isAdded = enabled && !wasEnabled;
              const isRemoved = !enabled && wasEnabled;
              return (
                <div key={perm.key} className="flex items-start gap-2">
                  <Switch
                    id={`${user.id}-${perm.key}`}
                    checked={enabled}
                    disabled={
                      user.is_super ||
                      isSelf ||
                      !canUpdate ||
                      (!isCurrentUserSuper &&
                        !currentUserPermissions.includes(perm.key))
                    }
                    onCheckedChange={(checked) =>
                      onToggle(user, perm.key, checked)
                    }
                    className="mt-0.5 scale-75"
                  />
                  <Label
                    htmlFor={`${user.id}-${perm.key}`}
                    className="flex flex-col items-start cursor-pointer text-xs leading-tight"
                  >
                    <span className="flex items-center gap-1.5">
                      {perm.name}
                      {isAdded && (
                        <span className="inline-block size-1.5 rounded-full bg-green-500" />
                      )}
                      {isRemoved && (
                        <span className="inline-block size-1.5 rounded-full bg-red-500" />
                      )}
                    </span>
                    <span className="text-muted-foreground font-normal">
                      {perm.description}
                    </span>
                  </Label>
                </div>
              );
            })}
          </div>
        </div>
      ))}
    </div>
  );
}

function TransferOwnershipDialog({
  user,
  disabled,
  onConfirm,
}: {
  user: UserSummary;
  disabled?: boolean;
  onConfirm: () => void;
}) {
  const [open, setOpen] = useState(false);

  async function handleConfirm() {
    onConfirm();
    setOpen(false);
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={setOpen}>
      <ResponsiveDialogTrigger asChild>
        <Button variant="outline" size="sm" disabled={disabled}>
          <Shield className="mr-1 size-3.5" />
          Transfer Ownership
        </Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Transfer Ownership</ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            Are you sure you want to transfer ownership to{" "}
            <span className="font-mono text-sm">{user.did}</span>? You will lose
            your owner privileges and cannot undo this action without their
            cooperation.
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>
        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </ResponsiveDialogClose>
          <Button variant="destructive" onClick={handleConfirm}>
            Transfer
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}

function AddUserDialog({
  onSuccess,
  templates,
  templatePermissions,
}: {
  onSuccess: () => void;
  templates: PermissionTemplate[];
  templatePermissions: Record<string, string[]>;
}) {
  const [did, setDid] = useState("");
  const [template, setTemplate] = useState<string>("");
  const [open, setOpen] = useState(false);

  async function handleAdd() {
    try {
      const body: { did: string; template?: string } = { did };
      if (template) body.template = template;
      await addUser(body);
      toast.success("User added");
      setDid("");
      setTemplate("");
      setOpen(false);
      onSuccess();
    } catch (e: unknown) {
      toastError("Failed to add user", e);
    }
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={setOpen}>
      <ResponsiveDialogTrigger asChild>
        <Button>Add User</Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>Add User</ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            Add a new user by their DID and optionally assign a permission
            template.
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>
        <div className="flex flex-col gap-4">
          <div className="flex flex-col gap-2">
            <Label htmlFor="user-did">DID</Label>
            <Input
              id="user-did"
              value={did}
              onChange={(e) => setDid(e.target.value)}
              placeholder="did:plc:..."
            />
          </div>
          <div className="flex flex-col gap-2">
            <Label htmlFor="user-template">Template</Label>
            <Select value={template} onValueChange={setTemplate}>
              <SelectTrigger id="user-template">
                <SelectValue placeholder="No template (no permissions)" />
              </SelectTrigger>
              <SelectContent>
                {templates.map((t) => (
                  <SelectItem key={t.key} value={t.key}>
                    {t.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          {template && (
            <p className="text-muted-foreground text-xs">
              Grants {templatePermissions[template]?.length ?? 0} permissions.
            </p>
          )}
        </div>
        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant="outline">Cancel</Button>
          </ResponsiveDialogClose>
          <Button onClick={handleAdd} disabled={!did.trim()}>
            Add
          </Button>
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
