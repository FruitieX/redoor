import * as React from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";

import { ConfirmationDialog } from "#ui/components/confirmation-dialog";
import { editorBufferQueryOptions } from "#ui/editor/stored-buffer";
import { queryKeys } from "#ui/queries";
import {
    getOpenFileKey,
    openFile,
    useUserState,
    type OpenFile,
} from "#ui/user-state";
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
