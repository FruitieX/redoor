import * as React from "react";
import { useQueryClient } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { useSetAtom } from "jotai";

import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { DeletePathsDialog } from "#ui/components/browser/delete-paths-dialog";
import { getImmediateParentPath } from "#ui/components/browser/utils";
import { invalidateAgentData } from "#ui/queries";
import { unselectFileAtom } from "#ui/selected-files";
import { closeFile, getOpenFileKey, useUserState } from "#ui/user-state";

const rootRouteApi = getRouteApi("__root__");

/**
 * Confirms deleting one path and owns what every surface must do once it is gone, so the
 * file list, the details view and the editor cannot drift apart: the path leaves the
 * selection and the open files (with its editor buffer), cached device data refreshes, and
 * a page that showed the path itself moves to its parent. Callers mount it only while the
 * confirmation is open.
 */
export function DeletePathDialog(props: {
    agent: Agent;
    path: string;
    name: string;
    entryType: "file" | "directory";
    /** Leaves the deleted path's own page for its parent, which is where the user can go on. */
    leaveToParent?: boolean;
    /** Lets a surface drop state tied to the path, such as an editor draft, before leaving. */
    onDeleted?: () => void;
    /** Adds surface-specific warnings below the path, such as unsaved edits. */
    children?: React.ReactNode;
    onClose: () => void;
}) {
    const navigate = useNavigate();
    const queryClient = useQueryClient();
    const unselectFile = useSetAtom(unselectFileAtom);
    const [, setUserState] = useUserState();
    const { editor } = rootRouteApi.useRouteContext();

    /** Removes the path everywhere it is remembered, then moves off its page if it had one. */
    const cleanUp = async () => {
        const target = { agentId: props.agent.id, path: props.path };

        // A removed path must disappear from navigation and pending selection actions.
        unselectFile(target);
        // The file is gone, so its unsaved edits go with it (the delete confirmation said so).
        editor.close(getOpenFileKey(target));
        setUserState((current) => ({
            ...current,
            openFiles: closeFile(current.openFiles, target),
        }));
        props.onDeleted?.();

        const parentPath = getImmediateParentPath(props.path);

        // Leave before refreshing, so no mounted query refetches the path that no longer exists.
        if (props.leaveToParent === true && parentPath !== null) {
            await navigate({
                ...browserRoute(props.agent, parentPath),
                replace: true,
            });
        }

        await invalidateAgentData(queryClient, props.agent.id);
    };

    return (
        <DeletePathsDialog
            title={`Delete this ${props.entryType}?`}
            description={
                props.agent.supportsTrash
                    ? `Move ${props.name} to the device trash. You can restore it later from the Trash tab.`
                    : `Move ${props.name} to the device's native Trash.`
            }
            targets={[{ agent: props.agent, path: props.path }]}
            trashConfirmLabel="Move to trash"
            permanentConfirmLabel={`Delete ${props.entryType}`}
            onClose={props.onClose}
            onDeleted={cleanUp}
        >
            <p className="overflow-x-auto whitespace-nowrap rounded-md border border-slate-800 bg-canvas px-3 py-2.5 font-mono text-sm text-slate-300">
                {props.path}
            </p>
            {props.children}
        </DeletePathsDialog>
    );
}
