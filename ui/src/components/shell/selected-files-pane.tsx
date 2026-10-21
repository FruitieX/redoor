import { useAtomValue, useSetAtom } from "jotai";
import { Link } from "@tanstack/react-router";
import { X } from "lucide-react";

import type { Agent } from "#ui/api-client";
import { Button } from "#ui/components/button";
import { IconButton } from "#ui/components/icon-button";
import { useAgents } from "#ui/use-agents";
import {
    clearSelectedFilesAtom,
    selectedFilesAtom,
    unselectFileAtom,
    type SelectedPath,
} from "#ui/selected-files";

/** Keeps the drawer tab's count in a leaf so selection changes never rerender the drawer. */
export function SelectedFilesBadge() {
    return <>{useAtomValue(selectedFilesAtom).length}</>;
}

/** Renders controls that operate on the complete selected-items list. */
function SelectedFilesPanelActions(props: {
    onClearSelectedFiles: () => void;
}) {
    return (
        <div className="flex items-center gap-2">
            <Button
                type="button"
                variant="subtle"
                aria-label="Clear all selected items"
                onClick={props.onClearSelectedFiles}
                className="inline-flex h-8 items-center justify-center rounded-md px-2 text-xs font-medium text-slate-400 transition-colors hover:bg-tint hover:text-slate-100 sm:px-2.5"
            >
                <X className="h-4 w-4 sm:hidden" aria-hidden="true" />
                <span className="hidden sm:inline">Clear all</span>
            </Button>
        </div>
    );
}

/** Displays selected paths in their scan-friendly order with per-item removal controls. */
function SelectedFilesTable(props: {
    agents: Agent[];
    selectedFiles: SelectedPath[];
    onUnselectFile: (file: Pick<SelectedPath, "agentId" | "path">) => void;
}) {
    return (
        <div className="h-full overflow-auto bg-surface">
            <ul className="divide-y divide-slate-800/70 md:hidden">
                {props.selectedFiles.map((file) => (
                    <li
                        key={`${file.agentId}:${file.path}`}
                        className="flex min-w-0 items-start gap-3 p-3"
                    >
                        <div className="min-w-0 flex-1">
                            <Link
                                to={
                                    props.agents
                                        .find(
                                            (agent) =>
                                                agent.id === file.agentId,
                                        )
                                        ?.getBrowserUrl(file.path) ?? "/"
                                }
                                className="block break-words text-sm font-medium text-blue-400 hover:underline"
                            >
                                {file.fileName}
                            </Link>
                            <p className="mt-1 text-xs text-slate-500">
                                {file.agentName}
                            </p>
                            <p className="mt-1 break-all font-mono text-xs leading-relaxed text-slate-400">
                                {file.path}
                            </p>
                        </div>
                        <IconButton
                            type="button"
                            label={`Unselect ${file.fileName}`}
                            tooltipClassName="shrink-0"
                            onClick={() =>
                                props.onUnselectFile({
                                    agentId: file.agentId,
                                    path: file.path,
                                })
                            }
                            className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-slate-700 text-slate-300 transition-colors hover:bg-tint"
                        >
                            <X className="h-4 w-4" aria-hidden="true" />
                        </IconButton>
                    </li>
                ))}
            </ul>
            <table className="hidden min-w-[44rem] w-full md:table">
                <thead className="sticky top-0 bg-surface-raised">
                    <tr className="border-b border-slate-800">
                        <th className="p-3 text-left text-sm font-medium text-slate-400">
                            Device
                        </th>
                        <th className="p-3 text-left text-sm font-medium text-slate-400">
                            Item
                        </th>
                        <th className="p-3 text-left text-sm font-medium text-slate-400">
                            Path
                        </th>
                        <th className="p-3 text-left text-sm font-medium text-slate-400">
                            Action
                        </th>
                    </tr>
                </thead>
                <tbody>
                    {props.selectedFiles.map((file) => (
                        <tr
                            key={`${file.agentId}:${file.path}`}
                            className="border-b border-slate-800/60 last:border-b-0 hover:bg-tint align-top"
                        >
                            <td className="p-3">
                                <div className="flex flex-col">
                                    <span className="text-sm font-medium text-slate-100">
                                        {file.agentName}
                                    </span>
                                    <span className="text-xs text-slate-500">
                                        {file.agentId}
                                    </span>
                                </div>
                            </td>
                            <td className="p-3">
                                <Link
                                    to={
                                        props.agents
                                            .find(
                                                (agent) =>
                                                    agent.id === file.agentId,
                                            )
                                            ?.getBrowserUrl(file.path) ?? "/"
                                    }
                                    className="text-sm font-medium text-blue-400 hover:underline"
                                >
                                    {file.fileName}
                                </Link>
                            </td>
                            <td className="p-3">
                                <div className="break-all font-mono text-xs text-slate-300">
                                    {file.path}
                                </div>
                            </td>
                            <td className="p-3">
                                <Button
                                    type="button"
                                    variant="secondary"
                                    aria-label={`Unselect ${file.fileName}`}
                                    onClick={() =>
                                        props.onUnselectFile({
                                            agentId: file.agentId,
                                            path: file.path,
                                        })
                                    }
                                    size="sm"
                                    className="py-1.5 text-xs"
                                >
                                    <X className="h-3.5 w-3.5" />
                                    Unselect
                                </Button>
                            </td>
                        </tr>
                    ))}
                </tbody>
            </table>
        </div>
    );
}

/**
 * Shows globally selected items inside the shared drawer without owning its chrome.
 */
export function SelectedFilesPane() {
    const agents = useAgents();
    const selectedFiles = useAtomValue(selectedFilesAtom);
    const unselectFile = useSetAtom(unselectFileAtom);
    const clearSelectedFiles = useSetAtom(clearSelectedFilesAtom);

    // Sort selected files case-insensitively with dot-prefixed items first so
    // the list is stable and easy to scan.
    const sortedSelectedFiles = [...selectedFiles].sort((a, b) => {
        const aIsDot = a.fileName.startsWith(".");
        const bIsDot = b.fileName.startsWith(".");

        if (aIsDot !== bIsDot) {
            return aIsDot ? -1 : 1;
        }

        return a.fileName.localeCompare(b.fileName, undefined, {
            sensitivity: "base",
        });
    });

    return (
        <div className="flex h-full min-h-0 flex-col">
            {selectedFiles.length > 0 ? (
                <div className="flex shrink-0 justify-end pb-2">
                    <SelectedFilesPanelActions
                        onClearSelectedFiles={clearSelectedFiles}
                    />
                </div>
            ) : null}
            <div className="min-h-0 flex-1 overflow-hidden rounded-md border border-slate-800">
                {selectedFiles.length > 0 ? (
                    <SelectedFilesTable
                        agents={agents}
                        selectedFiles={sortedSelectedFiles}
                        onUnselectFile={unselectFile}
                    />
                ) : (
                    <div className="flex h-full items-center justify-center px-4 text-center text-sm text-slate-500">
                        Select files or directories to review them here.
                    </div>
                )}
            </div>
        </div>
    );
}
