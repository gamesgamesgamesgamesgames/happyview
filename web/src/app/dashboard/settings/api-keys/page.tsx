"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import { Copy, Check } from "lucide-react";
import { toast } from "sonner";

import { useCurrentUser } from "@/hooks/use-current-user";
import { toastError } from "@/lib/format";
import {
  getApiKeys,
  createApiKey,
  revokeApiKey,
  getPermissions,
} from "@/lib/api";
import type { PermissionEntry } from "@/lib/api";
import type { ApiKeySummary, CreateApiKeyResponse } from "@/types/api-keys";
import { SiteHeader } from "@/components/site-header";
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
import { Button } from "@/components/ui/button";
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
import { Badge } from "@/components/ui/badge";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";

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

export default function ApiKeysPage() {
  const { hasPermission } = useCurrentUser();
  const [keys, setKeys] = useState<ApiKeySummary[]>([]);
  const [permissionEntries, setPermissionEntries] = useState<PermissionEntry[]>(
    [],
  );

  const load = useCallback(() => {
    getApiKeys()
      .then(setKeys)
      .catch((e) => toastError("Failed to load API keys", e));
  }, []);

  useEffect(() => {
    load();
    getPermissions()
      .then((catalog) => setPermissionEntries(catalog.permissions))
      .catch((e) => toastError("Failed to load permissions", e));
  }, [load]);

  async function handleRevoke(id: string) {
    try {
      await revokeApiKey(id);
      toast.success("API key revoked");
      load();
    } catch (e: unknown) {
      toastError("Failed to revoke API key", e);
    }
  }

  return (
    <>
      <SiteHeader title="API Keys" />
      <div className="flex flex-1 flex-col gap-4 p-4 md:p-6">
        <div className="flex items-center justify-between">
          <h2 className="text-lg font-semibold">API Keys</h2>
          {hasPermission("api-keys:create") && (
            <CreateApiKeyDialog
              onSuccess={load}
              permissions={permissionEntries}
            />
          )}
        </div>

        <div className="overflow-clip rounded-lg border">
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Key</TableHead>
                <TableHead>Permissions</TableHead>
                <TableHead>Created</TableHead>
                <TableHead>Last Used</TableHead>
                <TableHead className="w-10 sticky right-0 bg-inherit z-[1]" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {keys.length === 0 && (
                <TableRow>
                  <TableCell
                    colSpan={6}
                    className="text-muted-foreground text-center"
                  >
                    No API keys yet. Create an API key to allow external services to authenticate with this AppView.
                  </TableCell>
                </TableRow>
              )}
              {keys.map((key) => (
                <TableRow
                  key={key.id}
                  className={key.revoked_at ? "opacity-50" : undefined}
                >
                  <TableCell
                    className={key.revoked_at ? "line-through" : undefined}
                  >
                    {key.name}
                  </TableCell>
                  <TableCell className="font-mono text-sm">
                    {key.key_prefix}...
                  </TableCell>
                  <TableCell>
                    <Badge variant="secondary">
                      {key.permissions?.length ?? 0} perms
                    </Badge>
                  </TableCell>
                  <TableCell>
                    {new Date(key.created_at).toLocaleString()}
                  </TableCell>
                  <TableCell>
                    {key.last_used_at
                      ? new Date(key.last_used_at).toLocaleString()
                      : "Never"}
                  </TableCell>
                  <TableCell className="w-10 sticky right-0 bg-inherit z-[1]">
                    {!key.revoked_at && hasPermission("api-keys:delete") && (
                      <AlertDialog>
                        <AlertDialogTrigger asChild>
                          <Button variant="outline" size="sm">
                            Revoke
                          </Button>
                        </AlertDialogTrigger>
                        <AlertDialogContent>
                          <AlertDialogHeader>
                            <AlertDialogTitle>Revoke API key?</AlertDialogTitle>
                            <AlertDialogDescription>
                              This will permanently revoke the key &ldquo;
                              {key.name}&rdquo;. Any services using this key
                              will lose access.
                            </AlertDialogDescription>
                          </AlertDialogHeader>
                          <AlertDialogFooter>
                            <AlertDialogCancel>Cancel</AlertDialogCancel>
                            <AlertDialogAction
                              variant="destructive"
                              onClick={() => handleRevoke(key.id)}
                            >
                              Revoke
                            </AlertDialogAction>
                          </AlertDialogFooter>
                        </AlertDialogContent>
                      </AlertDialog>
                    )}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </div>
      </div>
    </>
  );
}

function CreateApiKeyDialog({
  onSuccess,
  permissions,
}: {
  onSuccess: () => void;
  permissions: PermissionEntry[];
}) {
  const categories = useMemo(
    () => buildCategories(permissions),
    [permissions],
  );
  const allKeys = useMemo(() => permissions.map((p) => p.key), [permissions]);

  const [name, setName] = useState("");
  const [selectedPermissions, setSelectedPermissions] = useState<string[]>([]);
  const [open, setOpen] = useState(false);
  const [createdKey, setCreatedKey] = useState<CreateApiKeyResponse | null>(
    null
  );
  const [copied, setCopied] = useState(false);

  function handleOpenChange(nextOpen: boolean) {
    setOpen(nextOpen);
    if (nextOpen) {
      setSelectedPermissions(allKeys);
    } else {
      setName("");
      setSelectedPermissions([]);
      if (createdKey) {
        setCreatedKey(null);
        onSuccess();
      }
    }
  }

  function togglePermission(perm: string) {
    setSelectedPermissions((prev) =>
      prev.includes(perm) ? prev.filter((p) => p !== perm) : [...prev, perm]
    );
  }

  function toggleCategory(perms: string[]) {
    const allSelected = perms.every((p) => selectedPermissions.includes(p));
    if (allSelected) {
      setSelectedPermissions((prev) => prev.filter((p) => !perms.includes(p)));
    } else {
      setSelectedPermissions((prev) => [
        ...prev,
        ...perms.filter((p) => !prev.includes(p)),
      ]);
    }
  }

  async function handleCreate() {
    try {
      const result = await createApiKey({
        name,
        permissions: selectedPermissions,
      });
      setCreatedKey(result);
      setCopied(false);
    } catch (e: unknown) {
      toastError("Failed to create API key", e);
    }
  }

  async function handleCopy() {
    if (!createdKey) return;
    await navigator.clipboard.writeText(createdKey.key);
    setCopied(true);
    setTimeout(() => setCopied(false), 2000);
  }

  return (
    <ResponsiveDialog open={open} onOpenChange={handleOpenChange}>
      <ResponsiveDialogTrigger asChild>
        <Button>Create API Key</Button>
      </ResponsiveDialogTrigger>
      <ResponsiveDialogContent>
        <ResponsiveDialogHeader>
          <ResponsiveDialogTitle>
            {createdKey ? "API Key Created" : "Create API Key"}
          </ResponsiveDialogTitle>
          <ResponsiveDialogDescription>
            {createdKey
              ? "Copy your API key now. It won\u2019t be shown again."
              : "Give this key a name to identify its purpose."}
          </ResponsiveDialogDescription>
        </ResponsiveDialogHeader>

        {createdKey ? (
          <div className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
              <Label>API Key</Label>
              <div className="flex gap-2">
                <Input
                  readOnly
                  value={createdKey.key}
                  className="font-mono text-sm"
                />
                <Button
                  variant="outline"
                  size="icon"
                  onClick={handleCopy}
                  title="Copy to clipboard"
                >
                  {copied ? (
                    <Check className="size-4" />
                  ) : (
                    <Copy className="size-4" />
                  )}
                </Button>
              </div>
              <p className="text-muted-foreground text-xs">
                Store this key securely. You will not be able to see it again.
              </p>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            <div className="flex flex-col gap-2">
              <Label htmlFor="api-key-name">Name</Label>
              <Input
                id="api-key-name"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g., CI Deploy"
              />
            </div>
            <div className="flex flex-col gap-2">
              <Label>Permissions</Label>
              <div className="max-h-64 overflow-y-auto rounded-md border p-3 flex flex-col gap-4">
                {Object.keys(categories).length === 0 && (
                  <p className="text-muted-foreground text-sm">
                    No permissions available.
                  </p>
                )}
                {Object.entries(categories).map(([category, perms]) => {
                  const keys = perms.map((p) => p.key);
                  const allSelected = keys.every((k) =>
                    selectedPermissions.includes(k)
                  );
                  const someSelected = keys.some((k) =>
                    selectedPermissions.includes(k)
                  );
                  return (
                    <div key={category} className="flex flex-col gap-2">
                      <button
                        type="button"
                        className="flex items-center gap-2 text-left"
                        onClick={() => toggleCategory(keys)}
                      >
                        <Checkbox
                          checked={allSelected}
                          data-state={
                            someSelected && !allSelected
                              ? "indeterminate"
                              : undefined
                          }
                          className="pointer-events-none"
                        />
                        <span className="text-sm font-medium">{category}</span>
                      </button>
                      <div className="ml-6 flex flex-col gap-1.5">
                        {perms.map((perm) => (
                          <label
                            key={perm.key}
                            className="flex items-start gap-2 cursor-pointer"
                          >
                            <Checkbox
                              checked={selectedPermissions.includes(perm.key)}
                              onCheckedChange={() => togglePermission(perm.key)}
                              className="mt-0.5"
                            />
                            <span className="flex flex-col">
                              <span className="text-sm">{perm.name}</span>
                              <span className="font-mono text-xs text-muted-foreground">
                                {perm.key}
                              </span>
                            </span>
                          </label>
                        ))}
                      </div>
                    </div>
                  );
                })}
              </div>
              <p className="text-muted-foreground text-xs">
                {selectedPermissions.length} of {allKeys.length} permissions
                selected
              </p>
            </div>
          </div>
        )}

        <ResponsiveDialogFooter>
          <ResponsiveDialogClose asChild>
            <Button variant={createdKey ? "default" : "outline"}>
              {createdKey ? "Done" : "Cancel"}
            </Button>
          </ResponsiveDialogClose>
          {!createdKey && (
            <Button onClick={handleCreate} disabled={!name.trim()}>
              Create
            </Button>
          )}
        </ResponsiveDialogFooter>
      </ResponsiveDialogContent>
    </ResponsiveDialog>
  );
}
