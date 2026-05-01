"use client";

import { useEffect, useState } from "react";
import {
  IconDashboard,
  IconFileDescription,
  IconDatabase,
  IconTable,
  IconClipboardList,
  IconUsers,
  IconLogout,
  IconKey,
  IconVariable,
  IconTag,
  IconPuzzle,
  IconSettings,
  IconInfoCircle,
  IconApps,
  IconArrowUpCircle,
  IconArrowsShuffle,
  IconCode,
  IconSkull,
} from "@tabler/icons-react";
import Image from "next/image";
import Link from "next/link";
import { usePathname } from "next/navigation";

import { useAuth } from "@/lib/auth-context";
import { useConfig } from "@/lib/config-context";
import { useCurrentUser } from "@/hooks/use-current-user";
import { usePluginUpdates } from "@/components/plugin-update-provider";
import { Scroller } from "@/components/ui/scroller";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarSeparator,
} from "@/components/ui/sidebar";

type NavItem = {
  title: string;
  url: string;
  icon: React.ComponentType;
  requiredPermissions?: string[];
};

const dataItems: NavItem[] = [
  { title: "Lexicons", url: "/dashboard/lexicons", icon: IconFileDescription },
  { title: "Records", url: "/dashboard/records", icon: IconTable },
  { title: "Backfill", url: "/dashboard/backfill", icon: IconDatabase },
  {
    title: "Dead Letters",
    url: "/dashboard/dead-letters",
    icon: IconSkull,
    requiredPermissions: ["dead-letters:read"],
  },
];

const accessItems: NavItem[] = [
  {
    title: "Users",
    url: "/dashboard/settings/users",
    icon: IconUsers,
    requiredPermissions: ["users:read"],
  },
  {
    title: "API Keys",
    url: "/dashboard/settings/api-keys",
    icon: IconKey,
    requiredPermissions: ["api-keys:read"],
  },
  {
    title: "API Clients",
    url: "/dashboard/settings/api-clients",
    icon: IconApps,
    requiredPermissions: ["api-clients:view"],
  },
];

const integrationItems: NavItem[] = [
  {
    title: "Plugins",
    url: "/dashboard/settings/plugins",
    icon: IconPuzzle,
    requiredPermissions: ["plugins:read"],
  },
  {
    title: "Labelers",
    url: "/dashboard/settings/labelers",
    icon: IconTag,
    requiredPermissions: ["labelers:read"],
  },
  {
    title: "Scripts",
    url: "/dashboard/settings/scripts",
    icon: IconCode,
    requiredPermissions: ["scripts:read"],
  },
];

const systemItems: NavItem[] = [
  {
    title: "General",
    url: "/dashboard/settings/general",
    icon: IconSettings,
    requiredPermissions: ["settings:manage"],
  },
  {
    title: "XRPC Proxy",
    url: "/dashboard/settings/xrpc-proxy",
    icon: IconArrowsShuffle,
    requiredPermissions: ["settings:manage"],
  },
  {
    title: "ENV Variables",
    url: "/dashboard/settings/env-variables",
    icon: IconVariable,
    requiredPermissions: ["script-variables:read"],
  },
  {
    title: "Event Logs",
    url: "/dashboard/events",
    icon: IconClipboardList,
    requiredPermissions: ["events:read"],
  },
];

export function AppSidebar({ ...props }: React.ComponentProps<typeof Sidebar>) {
  const pathname = usePathname();
  const { logout } = useAuth();
  const { app_name, logo_url } = useConfig();
  const { hasPermission } = useCurrentUser();
  const { hasUpdates } = usePluginUpdates();

  const [deadLetterCount, setDeadLetterCount] = useState(0);

  useEffect(() => {
    let cancelled = false;
    async function fetchCount() {
      try {
        const { getDeadLetterCount } = await import("@/lib/api");
        const data = await getDeadLetterCount("false");
        if (!cancelled) setDeadLetterCount(data.count);
      } catch {
        // sidebar badge is best-effort
      }
    }
    fetchCount();
    const interval = setInterval(fetchCount, 30000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  function filterByPermission(items: NavItem[]) {
    return items.filter(
      (item) =>
        !item.requiredPermissions ||
        item.requiredPermissions.some((perm) => hasPermission(perm)),
    );
  }

  function isActive(url: string) {
    return url === "/dashboard"
      ? pathname === "/dashboard"
      : pathname.startsWith(url);
  }

  const visibleData = filterByPermission(dataItems);
  const visibleAccess = filterByPermission(accessItems);
  const visibleIntegrations = filterByPermission(integrationItems);
  const visibleSystem = filterByPermission(systemItems);

  return (
    <Sidebar collapsible="offcanvas" {...props}>
      <SidebarHeader className="flex items-center justify-center p-4">
        {logo_url ? (
          <Image
            src={logo_url}
            alt={app_name ?? "HappyView"}
            width={140}
            height={48}
            className="object-contain"
            unoptimized
          />
        ) : (
          <>
            <Image
              src="/logo.light.png"
              alt="HappyView"
              width={140}
              height={48}
              className="block dark:hidden"
            />
            <Image
              src="/logo.dark.png"
              alt="HappyView"
              width={140}
              height={48}
              className="hidden dark:block"
            />
          </>
        )}
      </SidebarHeader>
      <SidebarSeparator className="!mx-0" />
      <Scroller asChild hideScrollbar>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupContent>
              <SidebarMenu>
                <SidebarMenuItem>
                  <SidebarMenuButton
                    asChild
                    tooltip="Dashboard"
                    isActive={isActive("/dashboard")}
                  >
                    <Link href="/dashboard">
                      <IconDashboard />
                      <span>Dashboard</span>
                    </Link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>

          {visibleData.length > 0 && (
            <SidebarGroup>
              <SidebarGroupLabel>Data</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {visibleData.map((item) => {
                    const showDeadLetterBadge =
                      item.title === "Dead Letters" && deadLetterCount > 0;
                    return (
                      <SidebarMenuItem key={item.title}>
                        <SidebarMenuButton
                          asChild
                          tooltip={item.title}
                          isActive={isActive(item.url)}
                        >
                          <Link href={item.url}>
                            <item.icon />
                            <span>{item.title}</span>
                            {showDeadLetterBadge && (
                              <span className="ml-auto rounded-full bg-destructive px-1.5 py-0.5 text-[10px] font-medium text-destructive-foreground">
                                {deadLetterCount}
                              </span>
                            )}
                          </Link>
                        </SidebarMenuButton>
                      </SidebarMenuItem>
                    );
                  })}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}

          {visibleAccess.length > 0 && (
            <SidebarGroup>
              <SidebarGroupLabel>Access</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {visibleAccess.map((item) => (
                    <SidebarMenuItem key={item.title}>
                      <SidebarMenuButton
                        asChild
                        tooltip={item.title}
                        isActive={isActive(item.url)}
                      >
                        <Link href={item.url}>
                          <item.icon />
                          <span>{item.title}</span>
                        </Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}

          {visibleIntegrations.length > 0 && (
            <SidebarGroup>
              <SidebarGroupLabel>Integrations</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {visibleIntegrations.map((item) => {
                    const showUpdateBadge =
                      item.title === "Plugins" && hasUpdates;
                    return (
                      <SidebarMenuItem key={item.title}>
                        <SidebarMenuButton
                          asChild
                          tooltip={item.title}
                          isActive={isActive(item.url)}
                        >
                          <Link href={item.url}>
                            <item.icon />
                            <span>{item.title}</span>
                            {showUpdateBadge && (
                              <IconArrowUpCircle
                                className="ml-auto size-4 text-primary"
                                aria-label="Updates available"
                              />
                            )}
                          </Link>
                        </SidebarMenuButton>
                      </SidebarMenuItem>
                    );
                  })}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}

          {visibleSystem.length > 0 && (
            <SidebarGroup>
              <SidebarGroupLabel>System</SidebarGroupLabel>
              <SidebarGroupContent>
                <SidebarMenu>
                  {visibleSystem.map((item) => (
                    <SidebarMenuItem key={item.title}>
                      <SidebarMenuButton
                        asChild
                        tooltip={item.title}
                        isActive={isActive(item.url)}
                      >
                        <Link href={item.url}>
                          <item.icon />
                          <span>{item.title}</span>
                        </Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          )}
        </SidebarContent>
      </Scroller>
      <SidebarSeparator className="!mx-0" />
      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton
              asChild
              tooltip="About"
              isActive={pathname === "/dashboard/about"}
            >
              <Link href="/dashboard/about">
                <IconInfoCircle />
                <span>About</span>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
          <SidebarMenuItem>
            <SidebarMenuButton onClick={logout} tooltip="Log out">
              <IconLogout />
              <span>Log out</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
    </Sidebar>
  );
}
