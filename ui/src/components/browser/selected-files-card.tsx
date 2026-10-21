import type { ResumeMode } from "#bindings/ResumeMode";
import React from "react";
import { useAtomValue, useSetAtom } from "jotai";
import {
    useMutation,
    useQueryClient,
    type QueryClient,
} from "@tanstack/react-query";
import { useRouterState } from "@tanstack/react-router";
import {
    Copy,
    Files,
    FolderInput,
    MoreHorizontal,
    PanelBottom,
    Trash2,
    X,
} from "lucide-react";
import { ActionMenu, ActionMenuButton } from "#ui/components/action-menu";
import { Button } from "#ui/components/button";
import {
    DeletePathsDialog,
    type DeletePathTarget,
} from "#ui/components/browser/delete-paths-dialog";
import { IconButton } from "#ui/components/icon-button";
import { Toast } from "#ui/components/toast";
import { Tooltip } from "#ui/components/tooltip";
import type { Agent, ApiClient } from "#ui/api-client";
import type { CopyExistingMode } from "#bindings/CopyExistingMode";
import { invalidateAgentData } from "#ui/queries";
import { waitForTransfers } from "#ui/transfer-wait";
import {
    selectedFilesAtom,
    clearSelectedFilesAtom,
    unselectFileAtom,
    type SelectedPath,
} from "#ui/selected-files";
import {
    isBrowserPathInside,
    joinBrowserPath,
} from "#ui/components/browser/utils";
import { getErrorMessage } from "#ui/utils/error";
import {
    SelectedFilesTransferTrigger,
    type SelectedFilesTransferOperation,
    type SelectedFilesTransferTriggerApi,
} from "#ui/components/browser/selected-files-transfer-dialog";
import { activateBottomDrawerTabAtom } from "#ui/bottom-drawer-state";
import { useIsBelowBreakpoint } from "#ui/utils/use-breakpoint";
import { AffectedPathsList } from "#ui/components/browser/affected-paths-list";

type TransferSelectedFilesState =
    | { type: "idle" }
    | { type: "transferring"; itemCount: number }
    | { type: "success"; message: string }
    | { type: "error"; message: string };

/**
 * Copy and Move share polling and selection cleanup so only the start API
 * and user-facing verbs differ.
 */
const transferActionCopy = {
    failed: "Copy failed",
    dismissAriaLabel: "Dismiss copy status",
    Icon: Copy,
};

const transferActionMove = {
    failed: "Move failed",
    dismissAriaLabel: "Dismiss move status",
    Icon: FolderInput,
};

/**
 * Waits on the shared transfers query so only finished items are unselected. Returns false
 * when the run was aborted and the caller must not update UI state.
 */
async function waitForPendingTransfers(props: {
    queryClient: QueryClient;
    api: ApiClient;
    pendingTransfers: Map<number, SelectedPath>;
    completedFiles: SelectedPath[];
    pausedFiles: SelectedPath[];
    destinationAgentId: string;
    directoryPath: string;
    failures: unknown[];
    failedLabel: string;
    signal: AbortSignal;
}): Promise<boolean> {
    if (props.pendingTransfers.size === 0) {
        return !props.signal.aborted;
    }

    try {
        return await waitForTransfers({
            queryClient: props.queryClient,
            api: props.api,
            signal: props.signal,
            isSettled: async (transfers) => {
                for (const transfer of transfers) {
                    const file = props.pendingTransfers.get(
                        transfer.request_id,
                    );

                    if (!file) continue;

                    if (transfer.state === "completed") {
                        props.completedFiles.push(file);
                        props.pendingTransfers.delete(transfer.request_id);
                    } else if (transfer.state === "errored") {
                        props.failures.push(
                            transfer.error ?? props.failedLabel,
                        );
                        props.pendingTransfers.delete(transfer.request_id);
                    }
                }

                const visibleIds = new Set(
                    transfers.map((transfer) => transfer.request_id),
                );

                const missingTransfers = [...props.pendingTransfers].filter(
                    ([id]) => !visibleIds.has(id),
                );

                if (missingTransfers.length > 0) {
                    // A paused copy drops its row, so its kept partial is the only trace.
                    const inventory = await props.api.listPartials();

                    for (const [id, file] of missingTransfers) {
                        const paused = inventory.partials.some(
                            (partial) =>
                                partial.source.agent === file.agentId &&
                                partial.source.path === file.path &&
                                partial.dest_agent_id ===
                                    props.destinationAgentId &&
                                partial.dest.path ===
                                    joinBrowserPath(
                                        props.directoryPath,
                                        file.fileName,
                                    ),
                        );

                        if (paused) {
                            props.pausedFiles.push(file);
                            props.pendingTransfers.delete(id);
                        }
                    }
                }

                return props.pendingTransfers.size === 0;
            },
        });
    } catch (error) {
        props.failures.push(error);

        return !props.signal.aborted;
    }
}

/**
 * Hides Copy/Move when every item would land on itself or a selected
 * directory would be nested inside its own tree.
 */
function canTransferSelectedFiles(props: {
    selectedFiles: SelectedPath[];
    destinationAgentId: string;
    directoryPath: string;
    isTransferring: boolean;
    isSiblingTransferring: boolean;
    isRoutePending: boolean;
}) {
    if (
        props.selectedFiles.length === 0 ||
        props.isTransferring ||
        props.isSiblingTransferring ||
        props.isRoutePending
    ) {
        return false;
    }

    const allLandOnThemselves = props.selectedFiles.every(
        (file) =>
            file.agentId === props.destinationAgentId &&
            file.path === joinBrowserPath(props.directoryPath, file.fileName),
    );

    if (allLandOnThemselves) {
        return false;
    }

    return !props.selectedFiles.some(
        (file) =>
            file.entryType === "directory" &&
            file.agentId === props.destinationAgentId &&
            isBrowserPathInside(file.path, props.directoryPath),
    );
}

/** Starts selected transfers through one mutation shared by copy and move. */
function useTransferStarts(props: {
    agents: Agent[];
    destinationAgent: Agent;
    directoryPath: string;
    operation: SelectedFilesTransferOperation;
}) {
    const isCopy = props.operation === "copy";

    return useMutation({
        mutationFn: (request: {
            filesToTransfer: SelectedPath[];
            existingMode: CopyExistingMode;
            resume: ResumeMode;
        }) => {
            const agentsById = new Map(
                props.agents.map((agent) => [agent.id, agent]),
            );

            return Promise.allSettled(
                request.filesToTransfer.map(async (file) => {
                    const sourceAgent = agentsById.get(file.agentId);

                    if (!sourceAgent) {
                        throw new Error(
                            `Source device unavailable for selected item: ${file.agentId}`,
                        );
                    }

                    const destination = {
                        agent: props.destinationAgent.id,
                        path: joinBrowserPath(
                            props.directoryPath,
                            file.fileName,
                        ),
                    };

                    const options = {
                        on_existing: request.existingMode,
                        resume: request.resume,
                    };

                    if (isCopy) {
                        const response = await sourceAgent.copyTo(
                            destination,
                            file.path,
                            options,
                        );

                        return response.copy_request_id;
                    }

                    const response = await sourceAgent.moveTo(
                        destination,
                        file.path,
                        options,
                    );

                    return response.move_request_id;
                }),
            );
        },
    });
}

/**
 * Starts every selected item into this directory so the destination is clear
 * at the point where Copy or Move is performed. Sibling in-flight state is
 * owned by the card so the other action cannot race the same sources.
 */
function TransferSelectedFilesAction(props: {
    operation: SelectedFilesTransferOperation;
    api: ApiClient;
    agents: Agent[];
    destinationAgent: Agent;
    directoryPath: string;
    destinationFileNames: string[];
    isSiblingTransferring: boolean;
    onTransferringChange: (isTransferring: boolean) => void;
    children: (trigger: SelectedFilesTransferTriggerApi) => React.ReactNode;
}) {
    const queryClient = useQueryClient();
    const selectedFiles = useAtomValue(selectedFilesAtom);
    const unselectFile = useSetAtom(unselectFileAtom);

    const isRoutePending = useRouterState({
        select: (state) => state.status === "pending",
    });

    const [transferState, setTransferState] =
        React.useState<TransferSelectedFilesState>({
            type: "idle",
        });

    const activeTransferRef = React.useRef<AbortController | null>(null);
    const isCopy = props.operation === "copy";
    const labels = isCopy ? transferActionCopy : transferActionMove;

    const transferStartsMutation = useTransferStarts(props);

    React.useEffect(() => {
        return () => activeTransferRef.current?.abort();
    }, []);

    const statusMessage =
        transferState.type === "transferring"
            ? `${isCopy ? "Copying" : "Moving"} ${transferState.itemCount} ${transferState.itemCount === 1 ? "item" : "items"}...`
            : transferState.type === "idle"
              ? null
              : transferState.message;

    const isTransferring = transferState.type === "transferring";

    const canTransfer = canTransferSelectedFiles({
        selectedFiles,
        destinationAgentId: props.destinationAgent.id,
        directoryPath: props.directoryPath,
        isTransferring,
        isSiblingTransferring: props.isSiblingTransferring,
        isRoutePending,
    });

    /** Starts every selected transfer with one consistent destination conflict policy. */
    const transferSelectedFiles = async (
        mode: CopyExistingMode,
        resume: ResumeMode,
    ) => {
        if (selectedFiles.length === 0) {
            return;
        }

        const filesToTransfer = [...selectedFiles];
        const controller = new AbortController();
        activeTransferRef.current?.abort();
        activeTransferRef.current = controller;

        setTransferState({
            type: "transferring",
            itemCount: filesToTransfer.length,
        });
        props.onTransferringChange(true);

        try {
            const results = await transferStartsMutation.mutateAsync({
                filesToTransfer,
                existingMode: mode,
                resume,
            });

            if (controller.signal.aborted) return;

            const completedFiles: SelectedPath[] = [];
            const pausedFiles: SelectedPath[] = [];
            const failures: unknown[] = [];

            const pendingTransfers = new Map(
                results.flatMap((result, index) => {
                    const file = filesToTransfer[index];

                    if (result.status === "rejected") {
                        failures.push(result.reason);

                        return [];
                    }

                    return file ? [[result.value, file] as const] : [];
                }),
            );

            const stillActive = await waitForPendingTransfers({
                queryClient,
                api: props.api,
                pendingTransfers,
                completedFiles,
                pausedFiles,
                destinationAgentId: props.destinationAgent.id,
                directoryPath: props.directoryPath,
                failures,
                failedLabel: labels.failed,
                signal: controller.signal,
            });

            if (!stillActive) return;
            activeTransferRef.current = null;

            completedFiles.forEach((file) => {
                unselectFile({
                    agentId: file.agentId,
                    path: file.path,
                });
            });

            if (failures.length > 0) {
                const failureMessage = getErrorMessage(
                    failures[0],
                    labels.failed,
                ).replace(/^Upload failed$/, labels.failed);

                setTransferState({
                    type: "error",
                    message:
                        completedFiles.length > 0
                            ? `${isCopy ? "Copied" : "Moved"} ${completedFiles.length} of ${filesToTransfer.length} items. ${failureMessage}`
                            : failureMessage,
                });

                return;
            }

            setTransferState({
                type: "success",
                message:
                    pausedFiles.length > 0
                        ? `Paused ${pausedFiles.length} ${pausedFiles.length === 1 ? "item" : "items"}`
                        : filesToTransfer.length === 1
                          ? `${isCopy ? "Copied" : "Moved"} ${filesToTransfer[0]?.fileName ?? "item"}`
                          : `${isCopy ? "Copied" : "Moved"} ${filesToTransfer.length} items`,
            });
        } catch (error) {
            if (controller.signal.aborted) return;
            setTransferState({
                type: "error",
                message: getErrorMessage(error, labels.failed),
            });
        } finally {
            // Sibling Copy/Move must become usable again even if this run aborted.
            props.onTransferringChange(false);
        }
    };

    return (
        <>
            <SelectedFilesTransferTrigger
                api={props.api}
                operation={props.operation}
                selectedFiles={selectedFiles}
                destinationAgent={props.destinationAgent}
                directoryPath={props.directoryPath}
                destinationFileNames={props.destinationFileNames}
                canTransfer={canTransfer}
                onConfirm={(mode, resume) =>
                    void transferSelectedFiles(mode, resume)
                }
            >
                {props.children}
            </SelectedFilesTransferTrigger>
            {statusMessage ? (
                <Toast
                    tone={
                        transferState.type === "transferring"
                            ? "info"
                            : transferState.type === "error"
                              ? "error"
                              : "success"
                    }
                    icon={<labels.Icon className="h-4 w-4" />}
                    dismissAriaLabel={labels.dismissAriaLabel}
                    onDismiss={() => setTransferState({ type: "idle" })}
                >
                    {statusMessage}
                </Toast>
            ) : null}
        </>
    );
}

/** Renders a compact transfer button so desktop can keep Copy and Move visible. */
function SelectedFilesTransferButton(props: {
    trigger: SelectedFilesTransferTriggerApi;
}) {
    if (!props.trigger.canTransfer) {
        return null;
    }

    return (
        <Tooltip content={props.trigger.labels.buttonTooltip}>
            <Button
                type="button"
                onClick={props.trigger.start}
                aria-label={props.trigger.labels.buttonAriaLabel}
                size="sm"
                className="h-9 rounded-md px-3.5 py-0 font-semibold shadow-sm shadow-blue-950/30"
            >
                <props.trigger.labels.Icon className="h-3.5 w-3.5" />
                {props.trigger.labels.buttonLabel}
            </Button>
        </Tooltip>
    );
}

/**
 * Keeps Copy, Move, and Delete off a second mobile row by parking them in one
 * Actions dialog, while desktop still shows the verbs inline.
 */
function SelectedFilesCardActions(props: {
    isMobile: boolean;
    canDelete: boolean;
    copyTrigger: SelectedFilesTransferTriggerApi;
    moveTrigger: SelectedFilesTransferTriggerApi;
    onDelete: () => void;
    onClear: () => void;
}) {
    const hasMenuActions =
        props.copyTrigger.canTransfer ||
        props.moveTrigger.canTransfer ||
        props.canDelete;

    return (
        <div className="flex shrink-0 items-center gap-1 sm:gap-2">
            {props.isMobile ? (
                hasMenuActions ? (
                    <ActionMenu
                        label="Actions"
                        icon={<MoreHorizontal className="h-4 w-4" />}
                        className="h-9 px-2.5 py-0"
                    >
                        {(close) => (
                            <>
                                {props.copyTrigger.canTransfer ? (
                                    <ActionMenuButton
                                        onClick={() => {
                                            close();
                                            props.copyTrigger.start();
                                        }}
                                    >
                                        <props.copyTrigger.labels.Icon className="h-4 w-4 text-slate-400" />
                                        {props.copyTrigger.labels.buttonLabel}
                                    </ActionMenuButton>
                                ) : null}
                                {props.moveTrigger.canTransfer ? (
                                    <ActionMenuButton
                                        onClick={() => {
                                            close();
                                            props.moveTrigger.start();
                                        }}
                                    >
                                        <props.moveTrigger.labels.Icon className="h-4 w-4 text-slate-400" />
                                        {props.moveTrigger.labels.buttonLabel}
                                    </ActionMenuButton>
                                ) : null}
                                {props.canDelete ? (
                                    <>
                                        <div className="my-1 border-t border-slate-800" />
                                        <ActionMenuButton
                                            tone="danger"
                                            onClick={() => {
                                                close();
                                                props.onDelete();
                                            }}
                                        >
                                            <Trash2 className="h-4 w-4" />
                                            Delete
                                        </ActionMenuButton>
                                    </>
                                ) : null}
                            </>
                        )}
                    </ActionMenu>
                ) : null
            ) : (
                <>
                    <SelectedFilesTransferButton trigger={props.copyTrigger} />
                    <SelectedFilesTransferButton trigger={props.moveTrigger} />
                    {props.canDelete ? (
                        <Tooltip content="Delete selected items">
                            <Button
                                type="button"
                                variant="danger"
                                aria-label="Delete selected items"
                                onClick={props.onDelete}
                                size="sm"
                                className="h-9 rounded-md border-red-500/40 bg-red-500/10 px-3.5 py-0 font-semibold text-red-200 hover:border-red-500/60 hover:bg-red-500/20"
                            >
                                <Trash2
                                    className="h-3.5 w-3.5"
                                    aria-hidden="true"
                                />
                                Delete
                            </Button>
                        </Tooltip>
                    ) : null}
                </>
            )}
            <IconButton
                type="button"
                label="Clear selection"
                onClick={props.onClear}
                className="inline-flex h-9 w-9 items-center justify-center rounded-md text-slate-400 transition-colors hover:bg-white/5 hover:text-slate-100"
            >
                <X className="h-4 w-4" aria-hidden="true" />
            </IconButton>
        </div>
    );
}

/** Keeps selected-item context and bulk actions adjacent to the file list. */
export function SelectedFilesCard(props: {
    api: ApiClient;
    agents: Agent[];
    destinationAgent: Agent;
    directoryPath: string;
    destinationFileNames: string[];
}) {
    const queryClient = useQueryClient();
    const selectedFiles = useAtomValue(selectedFilesAtom);
    const unselectFile = useSetAtom(unselectFileAtom);
    const clearSelectedFiles = useSetAtom(clearSelectedFilesAtom);
    const activateBottomDrawerTab = useSetAtom(activateBottomDrawerTabAtom);
    const [isDeleteDialogOpen, setIsDeleteDialogOpen] = React.useState(false);

    // One lock so Copy cannot start while Move is still reading or deleting sources.
    const [busyTransferOperation, setBusyTransferOperation] =
        React.useState<SelectedFilesTransferOperation | null>(null);

    const fileCount = selectedFiles.filter(
        (file) => file.entryType === "file",
    ).length;

    const directoryCount = selectedFiles.length - fileCount;
    const isMobile = useIsBelowBreakpoint("sm");
    const hasSelection = selectedFiles.length > 0;
    const canDelete = hasSelection;
    const selectionSummary = `${fileCount} ${fileCount === 1 ? "file" : "files"}, ${directoryCount} ${directoryCount === 1 ? "directory" : "directories"} selected`;

    // Clearing the selection dismisses the delete prompt. Resetting while rendering keeps it from
    // reappearing when something is selected again.
    if (isDeleteDialogOpen && !hasSelection) {
        setIsDeleteDialogOpen(false);
    }

    /** Opens confirm from both the desktop Delete button and the mobile Actions menu. */
    const openDeleteDialog = () => {
        setIsDeleteDialogOpen(true);
    };

    const agentsById = new Map(props.agents.map((agent) => [agent.id, agent]));

    const deleteTargets: DeletePathTarget[] = selectedFiles.map((file) => ({
        agent: agentsById.get(file.agentId) ?? null,
        path: file.path,
    }));

    return (
        <>
            <TransferSelectedFilesAction
                operation="copy"
                api={props.api}
                agents={props.agents}
                destinationAgent={props.destinationAgent}
                directoryPath={props.directoryPath}
                destinationFileNames={props.destinationFileNames}
                isSiblingTransferring={busyTransferOperation === "move"}
                onTransferringChange={(isTransferring) =>
                    setBusyTransferOperation(isTransferring ? "copy" : null)
                }
            >
                {(copyTrigger) => (
                    <TransferSelectedFilesAction
                        operation="move"
                        api={props.api}
                        agents={props.agents}
                        destinationAgent={props.destinationAgent}
                        directoryPath={props.directoryPath}
                        destinationFileNames={props.destinationFileNames}
                        isSiblingTransferring={busyTransferOperation === "copy"}
                        onTransferringChange={(isTransferring) =>
                            setBusyTransferOperation(
                                isTransferring ? "move" : null,
                            )
                        }
                    >
                        {(moveTrigger) => (
                            <section
                                aria-label="Selected files actions"
                                className="flex min-w-0 items-center gap-2 overflow-hidden border-b border-slate-800 bg-[#11141b] p-4 sm:p-5"
                            >
                                <span className="inline-flex h-9 w-9 shrink-0 items-center justify-center text-blue-400">
                                    <Files
                                        className="h-5 w-5"
                                        aria-hidden="true"
                                    />
                                </span>
                                {hasSelection ? (
                                    <Tooltip
                                        content="Show selected items"
                                        className="min-w-0 flex-1"
                                    >
                                        <Button
                                            type="button"
                                            variant="subtle"
                                            aria-label={`${selectionSummary}. Show selected items`}
                                            onClick={() =>
                                                activateBottomDrawerTab(
                                                    "selected",
                                                )
                                            }
                                            className="h-9 min-w-0 w-full justify-start gap-2 px-2 py-0 text-left text-sm font-medium text-slate-200 hover:bg-white/5 hover:text-white"
                                        >
                                            <span className="truncate">
                                                {selectionSummary}
                                            </span>
                                            <PanelBottom
                                                className="h-4 w-4 shrink-0 text-slate-400"
                                                aria-hidden="true"
                                            />
                                        </Button>
                                    </Tooltip>
                                ) : (
                                    <p className="flex h-9 min-w-0 flex-1 items-center truncate px-2 text-sm font-medium text-slate-200">
                                        {selectionSummary}
                                    </p>
                                )}
                                {hasSelection ? (
                                    <SelectedFilesCardActions
                                        isMobile={isMobile}
                                        canDelete={canDelete}
                                        copyTrigger={copyTrigger}
                                        moveTrigger={moveTrigger}
                                        onDelete={openDeleteDialog}
                                        onClear={clearSelectedFiles}
                                    />
                                ) : null}
                            </section>
                        )}
                    </TransferSelectedFilesAction>
                )}
            </TransferSelectedFilesAction>
            {isDeleteDialogOpen && hasSelection ? (
                <DeletePathsDialog
                    title={`Delete ${selectedFiles.length === 1 ? "this selected item" : "these selected items"}?`}
                    description={`Move ${fileCount} ${fileCount === 1 ? "file" : "files"} and ${directoryCount} ${directoryCount === 1 ? "directory" : "directories"} to trash.`}
                    targets={deleteTargets}
                    trashConfirmLabel={
                        selectedFiles.length === 1
                            ? "Move selected item to trash"
                            : `Move ${selectedFiles.length} selected items to trash`
                    }
                    permanentConfirmLabel={
                        selectedFiles.length === 1
                            ? "Delete selected item"
                            : `Delete ${selectedFiles.length} selected items`
                    }
                    requiredConfirmationText="DELETE"
                    onClose={() => setIsDeleteDialogOpen(false)}
                    onDeleted={async (targets) => {
                        targets.forEach((target) => {
                            const selected = selectedFiles.find(
                                (file) =>
                                    file.path === target.path &&
                                    file.agentId === target.agent?.id,
                            );

                            if (selected) {
                                unselectFile({
                                    agentId: selected.agentId,
                                    path: selected.path,
                                });
                            }
                        });

                        // Deleted items may live on several agents, including this directory's.
                        const agentIds = new Set(
                            targets.flatMap((target) =>
                                target.agent ? [target.agent.id] : [],
                            ),
                        );

                        await Promise.all(
                            [...agentIds].map((agentId) =>
                                invalidateAgentData(queryClient, agentId),
                            ),
                        );
                    }}
                >
                    <AffectedPathsList
                        paths={selectedFiles}
                        onDeselect={(path) =>
                            unselectFile({
                                agentId: path.agentId,
                                path: path.path,
                            })
                        }
                    />
                </DeletePathsDialog>
            ) : null}
        </>
    );
}
