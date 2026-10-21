import React from "react";
import { atom, useSetAtom, useStore } from "jotai";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useRouter } from "@tanstack/react-router";
import { queryKeys } from "#ui/queries";

type BrowserRouter = {
    invalidate: () => Promise<void>;
};

/** Names the open editor buffer and whether it holds unsaved text. */
type EditorRefreshState = {
    agentId: string;
    path: string;
    isDirty: boolean;
};

/**
 * Holds the editor that is open, so refresh triggers fired from anywhere (window focus,
 * leaving the terminal, the More menu) know whether a buffer must be refetched or protected.
 * It is an atom rather than a module variable, so it belongs to the store the application
 * renders with and every test gets a fresh one.
 */
const editorRefreshStateAtom = atom<EditorRefreshState | null>(null);

/**
 * Lets the mounted editor opt into shared browser reload triggers without a second listener.
 * Dirty buffers stay out of the refetch so unsaved text cannot be overwritten.
 */
export function useEditorRefreshRegistration(state: EditorRefreshState) {
    const setEditorState = useSetAtom(editorRefreshStateAtom);

    React.useEffect(() => {
        setEditorState({
            agentId: state.agentId,
            path: state.path,
            isDirty: state.isDirty,
        });

        return () =>
            // Only clear our own registration, in case another editor has already replaced it.
            setEditorState((current) =>
                current?.agentId === state.agentId &&
                current.path === state.path
                    ? null
                    : current,
            );
    }, [setEditorState, state.agentId, state.path, state.isDirty]);
}

/**
 * Reloads the route, whose loader refetches the listing and metadata, plus the agent's git
 * state, and the editor buffer only when the editor is still clean. Automatic triggers and
 * the More menu share this so dirty edits are never overwritten. The route is reloaded
 * because the query cache cannot tell whether the entry at this URL changed kind, such as a
 * missing path that now exists.
 */
async function refreshBrowserPath(options: {
    router: BrowserRouter;
    queryClient: QueryClient;
    agentId: string;
    editor: EditorRefreshState | null;
}) {
    const fileContent =
        options.editor === null
            ? undefined
            : { agentId: options.editor.agentId, path: options.editor.path };

    const isEditorDirty = options.editor?.isDirty === true;

    // Pull editor bytes before invalidate so a remount cannot drop an inactive refetch.
    if (fileContent && !isEditorDirty) {
        await options.queryClient.refetchQueries(
            {
                queryKey: queryKeys.fileContent(
                    fileContent.agentId,
                    fileContent.path,
                ),
                type: "all",
            },
            {
                // CodeMirror focus can request the same bytes during tab restoration.
                cancelRefetch: false,
            },
        );
    }

    // Git state can change outside redoor without changing the directory listing, and only
    // the loader's own queries are refetched by the route reload below.
    await options.queryClient.invalidateQueries(
        {
            queryKey: queryKeys.git(options.agentId),
        },
        {
            // Creating a file navigates while this refresh may already be in flight.
            cancelRefetch: false,
        },
    );
    await options.router.invalidate();
}

/** Serializes browser reload triggers so one focus transition cannot duplicate requests. */
export function useRefreshBrowserPath(agentId: string) {
    const router = useRouter();
    const queryClient = useQueryClient();
    const store = useStore();
    const refreshPromiseRef = React.useRef<Promise<void> | null>(null);

    return React.useCallback(() => {
        if (refreshPromiseRef.current) {
            return refreshPromiseRef.current;
        }

        const refreshPromise = refreshBrowserPath({
            router,
            queryClient,
            agentId,
            editor: store.get(editorRefreshStateAtom),
        }).finally(() => {
            if (refreshPromiseRef.current === refreshPromise) {
                refreshPromiseRef.current = null;
            }
        });

        refreshPromiseRef.current = refreshPromise;

        return refreshPromise;
    }, [agentId, queryClient, router, store]);
}

/** Reloads the browser after returning to the window or leaving a terminal. */
export function useBrowserRefreshTriggers(agentId: string) {
    const refreshBrowser = useRefreshBrowserPath(agentId);

    React.useEffect(() => {
        const refresh = () => {
            if (document.visibilityState === "hidden") {
                return;
            }

            void refreshBrowser();
        };

        /** Ignores focus movement inside the terminal while catching exits to adjacent controls. */
        const refreshAfterTerminalBlur = (event: FocusEvent) => {
            const target = event.target;

            if (!(target instanceof Element)) {
                return;
            }

            const terminal = target.closest("[data-terminal-input]");

            if (!terminal) {
                return;
            }

            const nextTarget = event.relatedTarget;

            if (nextTarget instanceof Node && terminal.contains(nextTarget)) {
                return;
            }

            refresh();
        };

        window.addEventListener("focus", refresh);
        window.addEventListener("visibilitychange", refresh);
        document.addEventListener("focusout", refreshAfterTerminalBlur);

        return () => {
            window.removeEventListener("focus", refresh);
            window.removeEventListener("visibilitychange", refresh);
            document.removeEventListener("focusout", refreshAfterTerminalBlur);
        };
    }, [refreshBrowser]);
}
