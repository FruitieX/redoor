import * as React from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import { FileCheck2, FilePlus2 } from "lucide-react";

import { ConfirmationDialog } from "#ui/components/confirmation-dialog";
import { IconButton } from "#ui/components/icon-button";
import { editorBufferQueryOptions } from "#ui/editor/stored-buffer";
import { queryKeys } from "#ui/queries";
import {
    getOpenFileKey,
    isFileOpen,
    openFile,
    openFileAtLine,
    useUserState,
    type OpenFile,
} from "#ui/user-state";
import { cn } from "#ui/utils/cn";
import { getErrorMessage } from "#ui/utils/error";

const rootRouteApi = getRouteApi("__root__");

/**
 * Opens the file without leaving it. The editor calls it on the first edit, so unsaved text
 * is kept and listed without a click, and on a double click, which is how a file is kept
 * open before it has been changed.
 */
export function useKeepFileOpen(props: { file: OpenFile }) {
    const [, setUserState] = useUserState();
    const file = props.file;

    return React.useCallback(() => {
        setUserState((current) => {
            const openFiles = openFile(current.openFiles, file);

            if (openFiles === current.openFiles) {
                return current;
            }

            return { ...current, openFiles };
        });
    }, [file, setUserState]);
}

/**
 * Closes open files, asking first when one of them has unsaved edits, since closing
 * discards its buffer. A file not visited since the page loaded has no buffer in memory, so
 * its stored copy is read to know. Returns the confirmation dialog for the caller to render.
 */
export function useCloseOpenFiles() {
    const { api, editor } = rootRouteApi.useRouteContext();
    const queryClient = useQueryClient();
    const [, setUserState] = useUserState();
    const [pending, setPending] = React.useState<OpenFile[] | null>(null);

    const close = (files: OpenFile[]) => {
        for (const file of files) {
            const key = getOpenFileKey(file);
            editor.close(key);
            queryClient.removeQueries({
                queryKey: queryKeys.editorBuffer(key),
            });
        }

        const closedKeys = new Set(files.map((file) => getOpenFileKey(file)));

        // The server deletes the buffers of files the new list no longer has.
        setUserState((current) => ({
            ...current,
            openFiles: current.openFiles.filter(
                (file) => !closedKeys.has(getOpenFileKey(file)),
            ),
        }));
    };

    const checkMutation = useMutation({
        mutationFn: async (files: OpenFile[]) => {
            const dirty = await Promise.all(
                files.map(async (file) => {
                    const key = getOpenFileKey(file);
                    const loaded = editor.isDirty(key);

                    if (loaded !== undefined) {
                        return loaded;
                    }

                    const stored = await queryClient.fetchQuery(
                        editorBufferQueryOptions(api, key),
                    );

                    return stored?.data.dirty === true;
                }),
            );

            return { files, anyDirty: dirty.includes(true) };
        },
        onSuccess: (result) => {
            if (result.anyDirty) {
                setPending(result.files);
            } else {
                close(result.files);
            }
        },
    });

    const names = pending?.map((file) => file.name).join(", ") ?? "";

    const dialog = (
        <>
            <ConfirmationDialog
                isOpen={pending !== null}
                title="Discard unsaved changes?"
                description={
                    pending !== null && pending.length > 1
                        ? `Some of the open files have unsaved changes. Closing them discards those changes.`
                        : `${names} has unsaved changes. Closing it discards them.`
                }
                confirmLabel="Discard changes"
                onClose={() => setPending(null)}
                onConfirm={() => {
                    if (pending !== null) {
                        close(pending);
                    }

                    setPending(null);
                }}
            />
            <ConfirmationDialog
                isOpen={checkMutation.isError}
                title="Could not close the file"
                description={getErrorMessage(
                    checkMutation.error,
                    "Could not check the file for unsaved changes",
                )}
                confirmLabel="Close anyway"
                onClose={() => checkMutation.reset()}
                onConfirm={() => {
                    const files = checkMutation.variables;
                    checkMutation.reset();

                    if (files !== undefined) {
                        close(files);
                    }
                }}
            />
        </>
    );

    return {
        requestClose: (files: OpenFile[]) => checkMutation.mutate(files),
        dialog,
    };
}

/**
 * Names one line request. `crypto.randomUUID` needs a secure context, which a device reached
 * over plain HTTP on a LAN is not; the id only has to differ from the previous request.
 */
function createRevealId() {
    return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

/** Whether a file is open and how to open it, at a line, without going to it. */
export type BackgroundOpen = {
    fileName: string;
    isOpen: boolean;
    open: (line?: number) => void;
};

/**
 * Opens a file from a diff or search result while the reader stays where they are, so they
 * can collect several files to work on later. A line is selected and centered the next time
 * the file is shown. Pass null for a result without a working file to open.
 */
export function useBackgroundOpen(
    file: OpenFile | null,
): BackgroundOpen | null {
    const [userState, setUserState] = useUserState();
    const isOpen = file !== null && isFileOpen(userState.openFiles, file);

    const open = React.useCallback(
        (line?: number) => {
            if (file === null) {
                return;
            }

            const reveal =
                line === undefined ? undefined : { line, id: createRevealId() };

            setUserState((current) => {
                const openFiles = openFileAtLine(
                    current.openFiles,
                    file,
                    reveal,
                );

                if (openFiles === current.openFiles) {
                    return current;
                }

                return { ...current, openFiles };
            });
        },
        [file, setUserState],
    );

    // Diff rows are memoized, so they get the same object until something they show changes.
    return React.useMemo(
        () => (file === null ? null : { fileName: file.name, isOpen, open }),
        [file, isOpen, open],
    );
}

/**
 * A small icon button that opens a file without leaving the page. Its icon and a tinted chip
 * show whether the file is already open; an open file can still be sent to another line, while
 * opening one that is already open without a line would do nothing, so it says so instead.
 */
export function OpenInBackgroundButton(props: {
    target: BackgroundOpen;
    /** The line to show when the file is visited, for results that point at one. */
    line?: number;
    className?: string;
}) {
    const name = props.target.fileName;
    const line = props.line;
    const isOpen = props.target.isOpen;
    const atLine = line === undefined ? "" : ` at line ${String(line)}`;

    const label = isOpen
        ? line === undefined
            ? `${name} is open`
            : `Show line ${String(line)} when ${name} is visited`
        : `Open ${name}${atLine} in the background`;

    const tooltip = isOpen
        ? line === undefined
            ? `${name} is already in the open files`
            : `${name} is already open. Click to show line ${String(line)} the next time it is visited`
        : `Add ${name} to the open files${atLine} without leaving this page`;

    const Icon = isOpen ? FileCheck2 : FilePlus2;

    return (
        <IconButton
            type="button"
            variant="ghost"
            size="xs"
            label={label}
            tooltip={tooltip}
            aria-disabled={isOpen && line === undefined ? true : undefined}
            onClick={() => {
                if (isOpen && line === undefined) {
                    return;
                }

                props.target.open(line);
            }}
            className={cn(
                "shrink-0",
                // A tinted, outlined chip rather than a color shift alone, so an open file
                // stands out from a closed one when scanning a long list of rows.
                isOpen &&
                    "bg-blue-500/15 text-blue-300 ring-1 ring-blue-500/50 ring-inset hover:bg-blue-500/25 hover:text-blue-200",
                props.className,
            )}
        >
            <Icon className="h-3.5 w-3.5" aria-hidden="true" />
        </IconButton>
    );
}
