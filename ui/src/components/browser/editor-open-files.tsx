import * as React from "react";
import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";

import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { useCloseOpenFiles } from "#ui/components/browser/open-file-actions";
import { IconButton } from "#ui/components/icon-button";
import { SortableItem, SortableList } from "#ui/components/sortable-list";
import { Tooltip } from "#ui/components/tooltip";
import { cn } from "#ui/utils/cn";
import {
    getOpenFileKey,
    reorderOpenFiles,
    useUserState,
    type OpenFile,
} from "#ui/user-state";
import { useAgents } from "#ui/use-agents";

/** Keeps open files reachable when the full-window editor covers the sidebar. */
export function EditorOpenFiles(props: { agentId: string; filePath: string }) {
    const [userState, setUserState] = useUserState();
    const agents = useAgents();
    const listRef = React.useRef<HTMLElement>(null);
    const closer = useCloseOpenFiles();

    if (userState.openFiles.length === 0) {
        return closer.dialog;
    }

    return (
        <>
            <SortableList
                itemIds={userState.openFiles.map((file) =>
                    getOpenFileKey(file),
                )}
                orientation="horizontal"
                label="Editor open files"
                listRef={listRef}
                getItemLabel={(itemId) =>
                    userState.openFiles.find(
                        (file) => getOpenFileKey(file) === itemId,
                    )?.name ?? itemId
                }
                onMove={(move) => {
                    setUserState((current) => {
                        const openFiles = reorderOpenFiles(
                            current.openFiles,
                            move,
                        );

                        if (openFiles === current.openFiles) {
                            return current;
                        }

                        return { ...current, openFiles };
                    });
                }}
            >
                <nav
                    ref={listRef}
                    aria-label="Editor open files"
                    className="shrink-0 overflow-x-auto border-b border-slate-800 px-3 py-2"
                >
                    <ul className="flex w-max min-w-full items-center gap-2">
                        {userState.openFiles.map((file) => (
                            <EditorOpenFile
                                key={getOpenFileKey(file)}
                                file={file}
                                agent={agents.find(
                                    (agent) => agent.id === file.agentId,
                                )}
                                isActive={
                                    file.agentId === props.agentId &&
                                    file.path === props.filePath
                                }
                                onClose={() => closer.requestClose([file])}
                            />
                        ))}
                    </ul>
                </nav>
            </SortableList>
            {closer.dialog}
        </>
    );
}

/** One tab of the strip; closing it asks first when the file has unsaved edits. */
function EditorOpenFile(props: {
    file: OpenFile;
    /** The open file's device, or undefined while it is absent from the inventory. */
    agent: Agent | undefined;
    isActive: boolean;
    onClose: () => void;
}) {
    // The stored name is only a label for a device the inventory no longer lists.
    const deviceName = props.agent?.name ?? props.file.agentName;

    return (
        <SortableItem id={getOpenFileKey(props.file)}>
            {(item) => (
                <li
                    {...item.dragProps}
                    aria-label={`Open file ${props.file.name} on ${deviceName}`}
                    ref={item.setNodeRef}
                    style={item.style}
                    className={cn(
                        "flex items-center rounded",
                        item.isDragging && "opacity-70",
                        item.isOver &&
                            !item.isDragging &&
                            "ring-1 ring-blue-400/60",
                        props.isActive
                            ? "bg-white/10 text-slate-100"
                            : "text-slate-300 hover:bg-tint hover:text-slate-100",
                    )}
                >
                    <Tooltip
                        content={
                            props.agent
                                ? `Open ${props.file.path} on ${deviceName}`
                                : `${deviceName} is not available`
                        }
                    >
                        {/* Links follow the device id, never the stored name, which another
                            device may have taken since the file was opened. */}
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
                        label={`Close open file ${props.file.name}`}
                        tooltip={`Close ${props.file.name}`}
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
