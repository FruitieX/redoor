import * as React from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import {
    getRouteApi,
    Link,
    useLocation,
    useRouter,
} from "@tanstack/react-router";
import {
    ArrowLeftRight,
    Home,
    LoaderCircle,
    LogOut,
    ScrollText,
    KeyRound,
    Users,
    X,
} from "lucide-react";

import { getBrowserUrl } from "#ui/api-paths";
import { Button } from "#ui/components/button";
import { IconButton } from "#ui/components/icon-button";
import { RestartButton } from "#ui/components/restart-button";
import { waitForRestart } from "#ui/utils/wait-for-restart";
import { SidebarModeToggle } from "#ui/components/sidebar-mode-toggle";
import { SideMenu } from "#ui/components/side-menu";
import { SortableItem, SortableList } from "#ui/components/sortable-list";
import { Tooltip } from "#ui/components/tooltip";
import {
    invalidateAllAgentData,
    queryKeys,
    serverInfoQueryOptions,
} from "#ui/queries";
import { useCloseSideMenu } from "#ui/side-menu-state";
import { useAgents } from "#ui/use-agents";
import {
    getPinnedFileKey,
    reorderPinnedFiles,
    unpinFile,
    useUserState,
    type PinnedFile,
} from "#ui/user-state";

const rootRouteApi = getRouteApi("__root__");

/** Places application-level destinations in the shared left-side presentation. */
export function ApplicationNavigation() {
    return (
        <SideMenu menu="application" placement="left" label="Application menu">
            <BrandMark />
            <ApplicationMenu />
        </SideMenu>
    );
}

/** Shares application destinations, server restart, and logout between desktop and mobile menus. */
function ApplicationMenu() {
    const { api } = rootRouteApi.useRouteContext();
    const closeMenu = useCloseSideMenu();
    const queryClient = useQueryClient();
    const [userState, setUserState] = useUserState();
    const agents = useAgents();

    const menuItems = [
        {
            to: "/",
            label: "Home",
            ariaLabel: "Server home",
            icon: Home,
            exact: true,
        },
        {
            to: "/agents",
            label: "Devices",
            ariaLabel: "Manage devices",
            icon: Users,
            exact: true,
        },
        { to: "/logs", label: "Server logs", icon: ScrollText, exact: true },
        // The history page has no children today, but keeps highlighting if one is added.
        {
            to: "/transfers",
            label: "Transfers",
            icon: ArrowLeftRight,
            exact: false,
        },
        {
            to: "/settings/tokens",
            label: "Access tokens",
            ariaLabel: "Manage access tokens",
            icon: KeyRound,
            exact: true,
        },
    ] as const;

    return (
        <nav
            aria-label="Application"
            className="mt-3 flex min-h-0 flex-1 flex-col gap-1"
        >
            {menuItems.map((item) => {
                const Icon = item.icon;

                return (
                    <Link
                        key={item.to}
                        to={item.to}
                        aria-label={
                            "ariaLabel" in item ? item.ariaLabel : undefined
                        }
                        activeOptions={{ exact: item.exact }}
                        activeProps={{ className: "bg-tint text-slate-100" }}
                        inactiveProps={{
                            className:
                                "text-slate-300 hover:bg-tint hover:text-slate-100",
                        }}
                        onClick={closeMenu}
                        className="flex items-center gap-2.5 rounded px-3 py-2.5 text-sm transition-colors"
                    >
                        <Icon
                            className="h-4 w-4 shrink-0 text-slate-400"
                            aria-hidden="true"
                        />
                        {item.label}
                    </Link>
                );
            })}
            {userState.pinnedFiles.length > 0 ? (
                <PinnedFiles
                    pinnedFiles={userState.pinnedFiles}
                    deviceNameByAgentId={
                        new Map(agents.map((agent) => [agent.id, agent.name]))
                    }
                    onRemove={(file) => {
                        setUserState((current) => ({
                            ...current,
                            pinnedFiles: unpinFile(current.pinnedFiles, file),
                        }));
                    }}
                    onMove={(move) => {
                        setUserState((current) => {
                            const pinnedFiles = reorderPinnedFiles(
                                current.pinnedFiles,
                                move,
                            );

                            if (pinnedFiles === current.pinnedFiles) {
                                return current;
                            }

                            return { ...current, pinnedFiles };
                        });
                    }}
                    onClear={() => {
                        setUserState((current) => ({
                            ...current,
                            pinnedFiles: [],
                        }));
                    }}
                />
            ) : null}
            <div className="mt-auto flex flex-col gap-1">
                <SidebarModeToggle />
                <RestartButton
                    target="server"
                    ariaLabel="Restart server"
                    className="flex w-full items-center justify-start gap-2.5 rounded px-3 py-2.5 text-left text-sm text-slate-300 hover:bg-tint hover:text-slate-100"
                    description="The server will restart and re-read its configuration. Connected agents reconnect automatically. In-flight transfers and terminals are interrupted."
                    restart={() => api.restartServer()}
                    waitUntilReady={() => {
                        let oldServerClosed = false;

                        return waitForRestart(async () => {
                            try {
                                await queryClient.fetchQuery({
                                    ...serverInfoQueryOptions(api),
                                    staleTime: 0,
                                });
                            } catch (error) {
                                oldServerClosed = true;
                                throw error;
                            }

                            if (!oldServerClosed) {
                                throw new Error(
                                    "Old server is still shutting down",
                                );
                            }

                            // A new server process may report different agents and files.
                            await Promise.all([
                                queryClient.invalidateQueries({
                                    queryKey: queryKeys.agents(),
                                    exact: true,
                                }),
                                invalidateAllAgentData(queryClient),
                            ]);
                        }, "Server did not come back after restart");
                    }}
                />
                <LogoutButton />
            </div>
        </nav>
    );
}

/**
 * Ends the session and reloads into the login page, so no cached server state or open
 * socket outlives the user's credentials.
 */
function LogoutButton() {
    const { api } = rootRouteApi.useRouteContext();
    const router = useRouter();

    const logoutMutation = useMutation({
        mutationFn: () => api.logout(),
        onSuccess: () => {
            window.location.replace(
                router.buildLocation({ to: "/login" }).href,
            );
        },
    });

    return (
        <Button
            type="button"
            variant="ghost"
            onClick={() => logoutMutation.mutate()}
            disabled={logoutMutation.isPending}
            className="flex w-full items-center justify-start gap-2.5 rounded px-3 py-2.5 text-left font-normal text-slate-300 disabled:cursor-wait disabled:opacity-60"
        >
            {logoutMutation.isPending ? (
                <LoaderCircle
                    className="h-4 w-4 shrink-0 animate-spin text-slate-400"
                    aria-hidden="true"
                />
            ) : (
                <LogOut
                    className="h-4 w-4 shrink-0 text-slate-400"
                    aria-hidden="true"
                />
            )}
            {logoutMutation.isPending ? "Logging out…" : "Log out"}
        </Button>
    );
}

/** Lists pinned files under Transfers so they stay available from every route. */
function PinnedFiles(props: {
    pinnedFiles: PinnedFile[];
    deviceNameByAgentId: Map<string, string>;
    onRemove: (file: PinnedFile) => void;
    onClear: () => void;
    onMove: (move: { activeId: string; overId: string }) => void;
}) {
    const listRef = React.useRef<HTMLUListElement>(null);

    return (
        <div className="mt-2 flex min-h-0 flex-1 flex-col">
            <div className="flex items-center justify-between gap-2 px-3 py-1">
                <span className="text-xs font-medium tracking-wide text-slate-500 uppercase">
                    Pinned
                </span>
                <Button
                    type="button"
                    variant="subtle"
                    aria-label="Clear all pinned files"
                    onClick={props.onClear}
                    className="px-1 py-0.5 text-xs font-normal text-slate-500 hover:text-slate-200"
                >
                    Clear
                </Button>
            </div>
            <SortableList
                itemIds={props.pinnedFiles.map((file) =>
                    getPinnedFileKey(file),
                )}
                orientation="vertical"
                label="Pinned files"
                listRef={listRef}
                getItemLabel={(itemId) =>
                    props.pinnedFiles.find(
                        (file) => getPinnedFileKey(file) === itemId,
                    )?.name ?? itemId
                }
                onMove={props.onMove}
            >
                <ul
                    ref={listRef}
                    aria-label="Pinned files"
                    className="flex min-h-0 flex-1 flex-col gap-0.5 overflow-y-auto"
                >
                    {props.pinnedFiles.map((file) => (
                        <PinnedFileRow
                            key={getPinnedFileKey(file)}
                            file={file}
                            deviceName={
                                props.deviceNameByAgentId.get(file.agentId) ??
                                file.agentName
                            }
                            onRemove={props.onRemove}
                        />
                    ))}
                </ul>
            </SortableList>
        </div>
    );
}

/** Lets the row sort after a deliberate drag while a normal click still opens the file. */
function PinnedFileRow(props: {
    file: PinnedFile;
    deviceName: string;
    onRemove: (file: PinnedFile) => void;
}) {
    const closeMenu = useCloseSideMenu();
    const pathname = useLocation({ select: (location) => location.pathname });
    const href = getBrowserUrl(props.file.agentId, props.file.path);
    const isActive = pathname === href;
    const itemId = getPinnedFileKey(props.file);

    return (
        <SortableItem id={itemId}>
            {(item) => (
                <li
                    {...item.dragProps}
                    aria-label={`Pinned file ${props.file.name} on ${props.deviceName}`}
                    ref={item.setNodeRef}
                    style={item.style}
                    className={`group flex w-full min-w-0 items-start rounded ${
                        item.isDragging ? "opacity-70" : ""
                    } ${
                        item.isOver && !item.isDragging
                            ? "ring-1 ring-blue-400/60"
                            : ""
                    } ${
                        isActive
                            ? "bg-tint text-slate-100"
                            : "text-slate-300 hover:bg-tint hover:text-slate-100"
                    }`}
                >
                    <Tooltip
                        className="min-w-0 flex-1"
                        content={`Open ${props.file.path}`}
                    >
                        <Link
                            to={href}
                            aria-label={props.file.name}
                            aria-current={isActive ? "page" : undefined}
                            onClick={closeMenu}
                            className="block w-full min-w-0 px-3 py-1"
                        >
                            <span className="block truncate text-sm">
                                {props.file.name}
                            </span>
                            <span className="block truncate text-[10px] leading-tight text-slate-500">
                                {props.deviceName}
                            </span>
                        </Link>
                    </Tooltip>
                    <IconButton
                        type="button"
                        label="Remove pinned file"
                        tooltip={`Remove ${props.file.name}`}
                        onClick={() => props.onRemove(props.file)}
                        variant="quiet"
                        size="xs"
                        className="mt-1 mr-1 shrink-0"
                    >
                        <X className="h-3 w-3" aria-hidden="true" />
                    </IconButton>
                </li>
            )}
        </SortableItem>
    );
}

/** Keeps product identity with application-level navigation instead of route context. */
function BrandMark() {
    return (
        <Link
            to="/"
            tabIndex={-1}
            className="mr-2 flex shrink-0 items-center gap-2 px-2 pb-2 text-slate-200 hover:text-slate-100"
        >
            <span className="relative h-12 w-12 shrink-0" aria-hidden="true">
                <img
                    src="/logo-dark-transparent.svg"
                    alt=""
                    className="theme-logo-dark absolute inset-0 h-12 w-12"
                />
                <img
                    src="/logo-light-transparent.svg"
                    alt=""
                    className="theme-logo-light absolute inset-0 h-12 w-12"
                />
            </span>
            <span className="text-2xl font-semibold tracking-tight">
                Redoor
            </span>
        </Link>
    );
}
