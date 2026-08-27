import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Monitor, Moon, Settings, Sun } from "lucide-react";

import type { GatewayStatus } from "@/api/types";
import { useApp } from "@/AppContext";
import { useCodeDeliveryStore } from "@/code/CodeDeliveryStore";
import { GithubAvatar } from "@/code/GithubAvatar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import { useTheme, type ThemeMode } from "@/theme";
import {
  railAccountIdentity,
  type RailAccountIdentity,
} from "./railAccountIdentity";

const THEME_OPTIONS: readonly { value: ThemeMode; label: string }[] = [
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
  { value: "system", label: "System" },
];

/**
 * The rail's account chip. Identity fills in from the model gateway or GitHub
 * when those exist; settings and theme live in the menu so they do not occupy
 * footer rows of their own.
 */
export function SidebarAccountMenu({
  defaultOpen = false,
}: {
  defaultOpen?: boolean;
}) {
  const { client } = useApp();
  const { mode, setMode } = useTheme();
  const navigate = useNavigate();
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const githubLogin = useCodeDeliveryStore(
    (state) => state.repositorySnapshot?.capability.viewer_login,
  );
  const identity = railAccountIdentity({ gateway, githubLogin });

  useEffect(() => {
    let cancelled = false;
    void client
      .getGatewayStatus()
      .then((status) => {
        if (!cancelled) setGateway(status);
      })
      .catch(() => {
        if (!cancelled) setGateway(null);
      });
    void useCodeDeliveryStore
      .getState()
      .loadRepositories(client)
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [client]);

  return (
    <SidebarAccountMenuPanel
      identity={identity}
      themeMode={mode}
      defaultOpen={defaultOpen}
      onSettings={() => void navigate({ to: "/settings" })}
      onThemeMode={setMode}
    />
  );
}

export function SidebarAccountMenuPanel({
  identity,
  themeMode,
  defaultOpen = false,
  onSettings,
  onThemeMode,
}: {
  identity: RailAccountIdentity;
  themeMode: ThemeMode;
  defaultOpen?: boolean;
  onSettings: () => void;
  onThemeMode: (mode: ThemeMode) => void;
}) {
  const ThemeIcon =
    themeMode === "light" ? Sun : themeMode === "dark" ? Moon : Monitor;

  return (
    <DropdownMenu defaultOpen={defaultOpen}>
      <DropdownMenuTrigger asChild>
        <button
          type="button"
          className="hover:bg-accent inline-flex min-h-8 w-full cursor-pointer items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-sm font-normal outline-none focus-visible:ring-3 focus-visible:ring-ring/25"
          aria-label="Account menu"
        >
          <AccountAvatar identity={identity} />
          <span className="min-w-0 flex-1">
            <span className="block truncate leading-4">{identity.title}</span>
            {identity.detail && (
              <span className="text-muted-foreground block truncate text-2xs leading-4">
                {identity.detail}
              </span>
            )}
          </span>
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="start"
        sideOffset={8}
        className="w-64"
      >
        <div className="flex items-center gap-2.5 px-2 py-2">
          <AccountAvatar identity={identity} />
          <div className="min-w-0 flex-1">
            <p className="truncate text-sm font-medium">{identity.title}</p>
            {identity.detail ? (
              <p className="text-muted-foreground truncate text-xs">
                {identity.detail}
              </p>
            ) : (
              <p className="text-muted-foreground truncate text-xs">
                Sign in from Settings
              </p>
            )}
          </div>
        </div>
        <DropdownMenuSeparator />
        <DropdownMenuItem className="gap-2" onSelect={onSettings}>
          <Settings />
          Settings
        </DropdownMenuItem>
        <DropdownMenuSub>
          <DropdownMenuSubTrigger className="gap-2">
            <ThemeIcon />
            Theme
          </DropdownMenuSubTrigger>
          <DropdownMenuSubContent>
            {THEME_OPTIONS.map((option) => (
              <DropdownMenuItem
                key={option.value}
                className="gap-2"
                onSelect={() => onThemeMode(option.value)}
              >
                <span
                  className={cn(
                    "size-1.5 rounded-full",
                    themeMode === option.value
                      ? "bg-foreground"
                      : "bg-transparent",
                  )}
                  aria-hidden
                />
                {option.label}
              </DropdownMenuItem>
            ))}
          </DropdownMenuSubContent>
        </DropdownMenuSub>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function AccountAvatar({ identity }: { identity: RailAccountIdentity }) {
  if (identity.githubLogin) {
    return <GithubAvatar login={identity.githubLogin} className="size-6" />;
  }
  if (identity.source === "local") {
    return (
      <span
        className="border-border size-6 shrink-0 rounded-full border"
        aria-hidden
      />
    );
  }
  return (
    <span
      className="bg-muted text-muted-foreground grid size-6 shrink-0 place-items-center rounded-full text-2xs font-semibold uppercase"
      aria-hidden
    >
      {identity.title.slice(0, 2)}
    </span>
  );
}
