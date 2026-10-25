import React from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useAtomValue, useSetAtom } from "jotai";
import {
    Archive,
    ArchiveRestore,
    Copy,
    Download,
    ExternalLink,
    LoaderCircle,
    MoreHorizontal,
    Pencil,
    Trash2,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { BookmarkMenuButton } from "#ui/components/browser/bookmark-action";
import { ActionMenu, ActionMenuButton } from "#ui/components/action-menu";
import { Button } from "#ui/components/button";
import { Checkbox } from "#ui/components/checkbox";
import { DeletePathDialog } from "#ui/components/browser/delete-path-dialog";
import { PathNameDialog } from "#ui/components/browser/path-name-dialog";
import { browserRoute } from "#ui/browser-route";
import { waitForTransfers } from "#ui/transfer-wait";
import { Toast } from "#ui/components/toast";
import { Tooltip } from "#ui/components/tooltip";
import {
    getImmediateParentPath,
    joinBrowserPath,
} from "#ui/components/browser/utils";
import { getErrorMessage } from "#ui/utils/error";
import { activateBottomDrawerTabAtom } from "#ui/bottom-drawer-state";
import { invalidateAgentData } from "#ui/queries";
import {
    selectedFileKeysAtom,
    toggleSelectedFileAtom,
} from "#ui/selected-files";

const rootRouteApi = getRouteApi("__root__");

/** Renames the current entry in a focused workflow and follows its new URL. */
function RenamePathDialog(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    view?: "details" | "sync";
    navigateAfterRename: boolean;
    onClose: () => void;
}) {
    const navigate = useNavigate();
    const queryClient = useQueryClient();
    const parentPath = getImmediateParentPath(props.path);

    const renameMutation = useMutation({
        mutationFn: (newName: string) => {
            if (parentPath === null) {
                throw new Error("The filesystem root cannot be renamed");
            }

            return props.agent.renamePath(
                parentPath,
                props.currentName,
                newName,
            );
        },
        onSuccess: async (_, newName) => {
            if (parentPath === null) {
                return;
            }

            props.onClose();

            if (!props.navigateAfterRename) {
                await invalidateAgentData(queryClient, props.agent.id);

                return;
            }

            await navigate({
                ...browserRoute(
                    props.agent,
                    joinBrowserPath(parentPath, newName),
                ),
                search: { view: props.view },
            });
        },
    });

    return (
        <PathNameDialog
            title={`Rename ${props.entryType}`}
            description={`Choose a new name for ${props.currentName}.`}
            closeAriaLabel={`Close rename ${props.entryType} dialog`}
            kind={props.entryType === "file" ? "File" : "Directory"}
            fieldLabel="New name"
            fieldAriaLabel={`Rename ${props.entryType}`}
            placeholder={props.currentName}
            fieldDescription="The entry keeps its contents and moves to this name."
            initialName={props.currentName}
            requireChange
            selectNameStem
            unavailableMessage={
                parentPath === null
                    ? "The filesystem root cannot be renamed"
                    : null
            }
            submitLabel="Rename"
            busyLabel="Renaming..."
            submitIcon={<Pencil className="h-4 w-4" />}
            mutation={renameMutation}
            onClose={props.onClose}
        />
    );
}

/** Owns the rename workflow so different surfaces can provide their own trigger. */
function RenamePathAction(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    view?: "details" | "sync";
    navigateAfterRename?: boolean;
    children: (action: {
        open: () => void;
        disabled: boolean;
        dialog: React.ReactNode;
    }) => React.ReactNode;
}) {
    const [isOpen, setIsOpen] = React.useState(false);
    const disabled = getImmediateParentPath(props.path) === null;

    return props.children({
        open: () => setIsOpen(true),
        disabled,
        dialog: isOpen ? (
            <RenamePathDialog
                agent={props.agent}
                path={props.path}
                currentName={props.currentName}
                entryType={props.entryType}
                view={props.view}
                navigateAfterRename={props.navigateAfterRename !== false}
                onClose={() => setIsOpen(false)}
            />
        ) : null,
    });
}

/** Copies the current entry beside its source so the original path stays in place. */
function DuplicatePathDialog(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    onClose: () => void;
}) {
    const { api } = rootRouteApi.useRouteContext();
    const queryClient = useQueryClient();
    const parentPath = getImmediateParentPath(props.path);

    const duplicateMutation = useMutation({
        mutationFn: async (newName: string) => {
            if (parentPath === null) {
                throw new Error("The filesystem root cannot be duplicated");
            }

            const response = await props.agent.copyTo(
                {
                    agent: props.agent.id,
                    path: joinBrowserPath(parentPath, newName),
                },
                props.path,
            );

            // Copy returns a request id before the new name exists, so wait for publication.
            await waitForTransfers({
                queryClient,
                api,
                isSettled: (transfers) => {
                    const transfer = transfers.find(
                        (entry) =>
                            entry.request_id === response.copy_request_id,
                    );

                    if (transfer?.state === "errored") {
                        throw new Error(transfer.error ?? "Duplicate failed");
                    }

                    return transfer?.state === "completed";
                },
            });
        },
        onSuccess: async () => {
            props.onClose();
            await invalidateAgentData(queryClient, props.agent.id);
        },
    });

    return (
        <PathNameDialog
            title={`Duplicate ${props.entryType}`}
            description={`Choose a name for the copy of ${props.currentName}.`}
            closeAriaLabel={`Close duplicate ${props.entryType} dialog`}
            kind={props.entryType === "file" ? "File" : "Directory"}
            fieldLabel="New name"
            fieldAriaLabel={`Duplicate ${props.entryType}`}
            placeholder={props.currentName}
            fieldDescription="The copy is created beside the original."
            initialName={props.currentName}
            requireChange
            selectNameStem
            unavailableMessage={
                parentPath === null
                    ? "The filesystem root cannot be duplicated"
                    : null
            }
            submitLabel="Duplicate"
            busyLabel="Duplicating..."
            submitIcon={<Copy className="h-4 w-4" />}
            mutation={duplicateMutation}
            onClose={props.onClose}
        />
    );
}

/** Owns the duplicate workflow so the overflow menu can close before the dialog opens. */
function DuplicatePathAction(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    children: (action: {
        open: () => void;
        disabled: boolean;
        dialog: React.ReactNode;
    }) => React.ReactNode;
}) {
    const [isOpen, setIsOpen] = React.useState(false);
    const disabled = getImmediateParentPath(props.path) === null;

    return props.children({
        open: () => setIsOpen(true),
        disabled,
        dialog: isOpen ? (
            <DuplicatePathDialog
                agent={props.agent}
                path={props.path}
                currentName={props.currentName}
                entryType={props.entryType}
                onClose={() => setIsOpen(false)}
            />
        ) : null,
    });
}

/** Owns native-open feedback so the menu can close without losing the toast. */
function OpenNativelyAction(props: {
    agent: Agent;
    path: string;
    children: (action: {
        open: () => void;
        isPending: boolean;
        toast: React.ReactNode;
    }) => React.ReactNode;
}) {
    const openMutation = useMutation({
        mutationFn: () => props.agent.openPath(props.path),
    });

    return props.children({
        open: () => openMutation.mutate(),
        isPending: openMutation.isPending,
        toast:
            openMutation.isSuccess || openMutation.isError ? (
                <Toast
                    tone={openMutation.isError ? "error" : "success"}
                    icon={<ExternalLink className="h-4 w-4" />}
                    dismissAriaLabel="Dismiss native open message"
                    onDismiss={() => openMutation.reset()}
                >
                    {openMutation.isError
                        ? getErrorMessage(
                              openMutation.error,
                              "Could not open the path",
                          )
                        : "Opened on the device"}
                </Toast>
            ) : null,
    });
}

/** Renders the shared path verbs so each surface only chooses which ones apply. */
function PathActionMenuItems(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    close: () => void;
    renameDisabled: boolean;
    onRename: () => void;
    duplicateDisabled: boolean;
    onDuplicate: () => void;
    showOpenNatively?: boolean;
    openNativelyPending?: boolean;
    onOpenNatively?: () => void;
    showSelect?: boolean;
    showDownload?: boolean;
    downloadUrl?: string;
    downloadName?: string;
    onDownloadDirectory?: () => void;
    showUnarchive?: boolean;
    onUnarchive?: () => void;
    onCreateArchive?: () => void;
    onDelete?: () => void;
    deleteDisabled?: boolean;
}) {
    return (
        <>
            {props.showOpenNatively &&
            props.agent.supportsNativeOpen &&
            props.onOpenNatively ? (
                <ActionMenuButton
                    disabled={props.openNativelyPending}
                    onClick={() => {
                        props.close();
                        props.onOpenNatively?.();
                    }}
                >
                    {props.openNativelyPending ? (
                        <LoaderCircle className="h-4 w-4 animate-spin text-slate-400" />
                    ) : (
                        <ExternalLink className="h-4 w-4 text-slate-400" />
                    )}
                    {props.openNativelyPending ? "Opening..." : "Open natively"}
                </ActionMenuButton>
            ) : null}
            <ActionMenuButton
                disabled={props.renameDisabled}
                onClick={() => {
                    props.close();
                    props.onRename();
                }}
            >
                <Pencil className="h-4 w-4 text-slate-400" />
                Rename
            </ActionMenuButton>
            <ActionMenuButton
                disabled={props.duplicateDisabled}
                onClick={() => {
                    props.close();
                    props.onDuplicate();
                }}
            >
                <Copy className="h-4 w-4 text-slate-400" />
                Duplicate
            </ActionMenuButton>
            {props.showDownload ? (
                props.entryType === "directory" ? (
                    <ActionMenuButton
                        onClick={() => {
                            props.close();
                            props.onDownloadDirectory?.();
                        }}
                    >
                        <Download className="h-4 w-4 text-slate-400" />
                        Download
                    </ActionMenuButton>
                ) : (
                    <ActionMenuButton asChild>
                        <a
                            href={props.downloadUrl}
                            download={props.downloadName}
                            onClick={props.close}
                        >
                            <Download className="h-4 w-4 text-slate-400" />
                            Download
                        </a>
                    </ActionMenuButton>
                )
            ) : null}
            {props.showUnarchive && props.onUnarchive ? (
                <ActionMenuButton
                    onClick={() => {
                        props.close();
                        props.onUnarchive?.();
                    }}
                >
                    <ArchiveRestore className="h-4 w-4 text-slate-400" />
                    Unarchive
                </ActionMenuButton>
            ) : null}
            {props.onCreateArchive ? (
                <ActionMenuButton
                    onClick={() => {
                        props.close();
                        props.onCreateArchive?.();
                    }}
                >
                    <Archive className="h-4 w-4 text-slate-400" />
                    Create archive
                </ActionMenuButton>
            ) : null}
            {props.showSelect ? (
                <SelectPathMenuButton
                    agent={props.agent}
                    path={props.path}
                    fileName={props.currentName}
                    entryType={props.entryType}
                    close={props.close}
                />
            ) : null}
            <BookmarkMenuButton
                close={props.close}
                bookmark={{
                    agentId: props.agent.id,
                    path: props.path,
                    name: props.currentName,
                    entryType: props.entryType,
                }}
            />
            {props.onDelete ? (
                <>
                    <div className="my-1 border-t border-slate-800" />
                    <ActionMenuButton
                        tone="danger"
                        disabled={props.deleteDisabled}
                        onClick={() => {
                            props.close();
                            props.onDelete?.();
                        }}
                    >
                        <Trash2 className="h-4 w-4" />
                        Delete {props.entryType}
                    </ActionMenuButton>
                </>
            ) : null}
        </>
    );
}

/** Owns rename and native-open dialogs so the overflow menu can close first. */
export function PathActionMenu(props: {
    label: string;
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    view?: "details" | "sync";
    navigateAfterRename?: boolean;
    showOpenNatively?: boolean;
    showSelect?: boolean;
    showDownload?: boolean;
    downloadUrl?: string;
    downloadName?: string;
    onDownloadDirectory?: () => void;
    showUnarchive?: boolean;
    onUnarchive?: () => void;
    onCreateArchive?: () => void;
    onDelete?: () => void;
}) {
    const canModify = getImmediateParentPath(props.path) !== null;

    return (
        <RenamePathAction
            agent={props.agent}
            path={props.path}
            currentName={props.currentName}
            entryType={props.entryType}
            view={props.view}
            navigateAfterRename={props.navigateAfterRename}
        >
            {(renameAction) => (
                <DuplicatePathAction
                    agent={props.agent}
                    path={props.path}
                    currentName={props.currentName}
                    entryType={props.entryType}
                >
                    {(duplicateAction) => (
                        <OpenNativelyAction
                            agent={props.agent}
                            path={props.path}
                        >
                            {(openNatively) => (
                                <>
                                    <ActionMenu
                                        label={props.label}
                                        icon={
                                            <MoreHorizontal className="h-4 w-4" />
                                        }
                                        variant="icon"
                                    >
                                        {(close) => (
                                            <PathActionMenuItems
                                                agent={props.agent}
                                                path={props.path}
                                                currentName={props.currentName}
                                                entryType={props.entryType}
                                                close={close}
                                                renameDisabled={
                                                    renameAction.disabled
                                                }
                                                onRename={renameAction.open}
                                                duplicateDisabled={
                                                    duplicateAction.disabled
                                                }
                                                onDuplicate={
                                                    duplicateAction.open
                                                }
                                                showOpenNatively={
                                                    props.showOpenNatively
                                                }
                                                openNativelyPending={
                                                    openNatively.isPending
                                                }
                                                onOpenNatively={
                                                    openNatively.open
                                                }
                                                showSelect={props.showSelect}
                                                showDownload={
                                                    props.showDownload
                                                }
                                                downloadUrl={props.downloadUrl}
                                                downloadName={
                                                    props.downloadName
                                                }
                                                onDownloadDirectory={
                                                    props.onDownloadDirectory
                                                }
                                                showUnarchive={
                                                    props.showUnarchive
                                                }
                                                onUnarchive={props.onUnarchive}
                                                onCreateArchive={
                                                    props.onCreateArchive
                                                }
                                                onDelete={props.onDelete}
                                                deleteDisabled={!canModify}
                                            />
                                        )}
                                    </ActionMenu>
                                    {renameAction.dialog}
                                    {duplicateAction.dialog}
                                    {openNatively.toast}
                                </>
                            )}
                        </OpenNativelyAction>
                    )}
                </DuplicatePathAction>
            )}
        </RenamePathAction>
    );
}

/** Lets any path menu add or remove the current entry without duplicating selection wiring. */
export function SelectPathMenuButton(props: {
    agent: Agent;
    path: string;
    fileName: string;
    entryType: "file" | "directory";
    close: () => void;
}) {
    const selectedFileKeys = useAtomValue(selectedFileKeysAtom);
    const toggleSelectedFile = useSetAtom(toggleSelectedFileAtom);
    const activateBottomDrawerTab = useSetAtom(activateBottomDrawerTabAtom);
    const isSelected = selectedFileKeys.has(`${props.agent.id}:${props.path}`);
    const label = isSelected ? "Unselect" : "Select";

    return (
        <Checkbox
            checked={isSelected}
            label={label}
            title={false}
            className="w-full px-3 py-2"
            onCheckedChange={() => {
                props.close();
                toggleSelectedFile({
                    agentId: props.agent.id,
                    agentName: props.agent.name,
                    path: props.path,
                    fileName: props.fileName,
                    entryType: props.entryType,
                });

                if (!isSelected) {
                    // Menu selection has no listing checkbox, so show the chosen path immediately.
                    activateBottomDrawerTab("selected");
                }
            }}
        >
            {label}
        </Checkbox>
    );
}

/** Keeps object-level actions stable while each representation supplies its own controls. */
export function PersistentPathActions(props: {
    agent: Agent;
    path: string;
    currentName: string;
    entryType: "file" | "directory";
    view?: "details" | "sync";
    downloadUrl: string;
    downloadName: string;
    /** Explains archive packaging when the download is a directory tarball. */
    downloadTooltip?: string;
    secondaryDownload?: boolean;
    /** Lets a view keep Reload next to Download without hiding it in More. */
    afterDownload?: React.ReactNode;
}) {
    const [isConfirmDeleteOpen, setIsConfirmDeleteOpen] = React.useState(false);

    const downloadLink = (
        <Button
            as="a"
            href={props.downloadUrl}
            download={props.downloadName}
            aria-label="Download"
            variant={props.secondaryDownload ? "subtle" : "primary"}
            className={
                props.secondaryDownload
                    ? "rounded-md px-3 text-slate-200 hover:bg-tint hover:text-slate-100"
                    : "rounded-md px-3.5 font-semibold shadow-sm shadow-blue-950/30"
            }
        >
            <Download className="h-4 w-4" aria-hidden="true" />
            <span className="hidden sm:inline">Download</span>
        </Button>
    );

    return (
        <>
            <div className="flex items-center gap-2">
                <Tooltip content={props.downloadTooltip ?? "Download"}>
                    {downloadLink}
                </Tooltip>
                {props.afterDownload}
                <PathActionMenu
                    label="More"
                    agent={props.agent}
                    path={props.path}
                    currentName={props.currentName}
                    entryType={props.entryType}
                    view={props.view}
                    showOpenNatively={true}
                    showSelect={true}
                    onDelete={() => setIsConfirmDeleteOpen(true)}
                />
            </div>
            {isConfirmDeleteOpen ? (
                <DeletePathDialog
                    agent={props.agent}
                    path={props.path}
                    name={props.currentName}
                    entryType={props.entryType}
                    leaveToParent
                    onClose={() => setIsConfirmDeleteOpen(false)}
                />
            ) : null}
        </>
    );
}
