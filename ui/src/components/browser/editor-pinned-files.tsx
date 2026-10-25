import * as React from "react";
import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";

import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { IconButton } from "#ui/components/icon-button";
import { SortableItem, SortableList } from "#ui/components/sortable-list";
import { Tooltip } from "#ui/components/tooltip";
import {
    getPinnedFileKey,
    reorderPinnedFiles,
    unpinFile,
    useUserState,
} from "#ui/user-state";
import { useAgents } from "#ui/use-agents";

/** Keeps pinned destinations reachable when the full-window editor covers the sidebar. */
export function EditorPinnedFiles(props: {
    agentId: string;
    filePath: string;
}) {
    const [userState, setUserState] = useUserState();
    const agents = useAgents();
    const listRef = React.useRef<HTMLElement>(null);

    if (userState.pinnedFiles.length === 0) {
        return null;
    }

    return (
        <SortableList
            itemIds={userState.pinnedFiles.map((file) =>
                getPinnedFileKey(file),
            )}
            orientation="horizontal"
            label="Editor pinned files"
            listRef={listRef}
            getItemLabel={(itemId) =>
                userState.pinnedFiles.find(
                    (file) => getPinnedFileKey(file) === itemId,
                )?.name ?? itemId
            }
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
        >
            <nav
                ref={listRef}
                aria-label="Editor pinned files"
                className="shrink-0 overflow-x-auto border-b border-slate-800 px-3 py-2"
            >
                <ul className="flex w-max min-w-full items-center gap-2">
                    {userState.pinnedFiles.map((file) => (
                        <EditorPin
                            key={getPinnedFileKey(file)}
                            file={file}
                            agent={agents.find(
                                (agent) => agent.id === file.agentId,
                            )}
                            isActive={
                                file.agentId === props.agentId &&
                                file.path === props.filePath
                            }
                            onClose={() => {
                                setUserState((current) => ({
                                    ...current,
                                    pinnedFiles: unpinFile(
                                        current.pinnedFiles,
                                        file,
                                    ),
                                }));
                            }}
                        />
                    ))}
                </ul>
            </nav>
        </SortableList>
    );
}

/** Leaves the open editor and its unsaved buffer intact when a pin is closed. */
function EditorPin(props: {
    file: {
        agentId: string;
        path: string;
        name: string;
        agentName: string;
    };
    /** The pinned file's device, or undefined while it is absent from the inventory. */
    agent: Agent | undefined;
    isActive: boolean;
    onClose: () => void;
}) {
    // The stored name is only a label for a device the inventory no longer lists.
    const deviceName = props.agent?.name ?? props.file.agentName;

    return (
        <SortableItem id={getPinnedFileKey(props.file)}>
            {(item) => (
                <li
                    {...item.dragProps}
                    aria-label={`Pinned file ${props.file.name} on ${deviceName}`}
                    ref={item.setNodeRef}
                    style={item.style}
                    className={`flex items-center rounded ${
                        item.isDragging ? "opacity-70" : ""
                    } ${
                        item.isOver && !item.isDragging
                            ? "ring-1 ring-blue-400/60"
                            : ""
                    } ${
                        props.isActive
                            ? "bg-white/10 text-slate-100"
                            : "text-slate-300 hover:bg-tint hover:text-slate-100"
                    }`}
                >
                    <Tooltip
                        content={
                            props.agent
                                ? `Open ${props.file.path} on ${deviceName}`
                                : `${deviceName} is not available`
                        }
                    >
                        {/* Links follow the device id, never the stored name, which another
                            device may have taken since the file was pinned. */}
                        {props.agent ? (
                            <Link
                                {...browserRoute(props.agent, props.file.path)}
                                aria-label={props.file.name}
                                aria-current={
                                    props.isActive ? "page" : undefined
                                }
                                className="block px-3 py-1 text-sm whitespace-nowrap"
                            >
                                {props.file.name}
                            </Link>
                        ) : (
                            <span
                                aria-label={props.file.name}
                                aria-disabled="true"
                                tabIndex={0}
                                className="block px-3 py-1 text-sm whitespace-nowrap opacity-60"
                            >
                                {props.file.name}
                            </span>
                        )}
                    </Tooltip>
                    <IconButton
                        type="button"
                        label={`Close pinned file ${props.file.name}`}
                        tooltip={`Unpin ${props.file.name}`}
                        onClick={props.onClose}
                        variant="quiet"
                        size="xs"
                        className="mr-1 shrink-0"
                    >
                        <X className="h-3 w-3" aria-hidden="true" />
                    </IconButton>
                </li>
            )}
        </SortableItem>
    );
}
