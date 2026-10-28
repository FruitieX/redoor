import React from "react";
import { useAtom, useAtomValue } from "jotai";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import {
    ClipboardCopy,
    Download,
    Image as ImageIcon,
    LoaderCircle,
    MoreHorizontal,
    RefreshCw,
    Replace,
    Save,
    Search,
    ScanText,
    Terminal,
    Trash2,
    X,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import {
    bottomDrawerActiveTabAtom,
    bottomDrawerCollapsedAtom,
} from "#ui/bottom-drawer-state";
import { shortcutPriority, useShortcut } from "#ui/shortcuts";
import { useActiveTerminalTarget, useTerminals } from "#ui/use-terminals";
import { ActionMenu, ActionMenuButton } from "#ui/components/action-menu";
import { BrowserViewCard } from "#ui/components/browser-view-card";
import { ViewCardHeader } from "#ui/components/view-card-header";
import { Button } from "#ui/components/button";
import { BookmarkMenuButton } from "#ui/components/browser/bookmark-action";
import { useKeepFileOpen } from "#ui/components/browser/open-file-actions";
import {
    CodeEditor,
    type EditorSelection,
} from "#ui/components/browser/code-editor";
import type { EditorSearchHandle } from "#ui/components/browser/editor-search";
import {
    PersistentPathActions,
    SelectPathMenuButton,
} from "#ui/components/browser/path-actions";
import { getImmediateParentPath } from "#ui/components/browser/utils";
import { getErrorMessage } from "#ui/utils/error";
import { DeletePathDialog } from "#ui/components/browser/delete-path-dialog";
import { Checkbox } from "#ui/components/checkbox";
import { FullWindowToggle } from "#ui/components/full-window-toggle";
import { EditorOpenFiles } from "#ui/components/browser/editor-open-files";
import { IconButton } from "#ui/components/icon-button";
import { Toast } from "#ui/components/toast";
import { ToggleButton } from "#ui/components/toggle-button";
import {
    fileContentQueryOptions,
    gitRevisionContentQueryOptions,
} from "#ui/queries";
import { useAgentSearchNavigation } from "#ui/search-navigation";
import { useEditorRefreshRegistration } from "#ui/components/browser/refresh";
import { isTerminalInputTarget } from "#ui/utils/keyboard";
import {
    getOpenFileKey,
    useUserState,
    type OpenFileReveal,
} from "#ui/user-state";
import {
    editorBufferQueryOptions,
    type StoredBuffer,
} from "#ui/editor/stored-buffer";
import { syntaxLanguageFromFileName } from "#ui/utils/editor-language";
import { MarkdownPreview } from "#ui/components/browser/markdown-preview";

const rootRouteApi = getRouteApi("__root__");

/** Keeps save state and editor mutations inside the representation they affect. */
function FileEditActions(props: {
    /** A committed revision cannot be saved. */
    readOnly: boolean;
    statusMessage: string | null;
    hasError: boolean;
    isSaved: boolean;
    canEdit: boolean;
    isDirty: boolean;
    isSaving: boolean;
    bookmark: {
        agentId: string;
        path: string;
        name: string;
        entryType: "file";
    };
    selection: EditorSelection | null;
    preview?: boolean;
    isSearchOpen: boolean;
    onSave: () => void;
    onToggleSearch: () => void;
    onPreviewChange?: (preview: boolean) => void;
}) {
    const { updateSearch } = useAgentSearchNavigation();

    const copyMutation = useMutation({
        mutationFn: async () => {
            if (props.selection === null) {
                throw new Error("Select text in the editor before copying");
            }

            const reference = `\`\`\`${props.bookmark.path}#L${props.selection.startLine}
${props.selection.text}
\`\`\``;

            await navigator.clipboard.writeText(reference);
        },
    });

    return (
        <>
            {props.readOnly ? null : (
                <Button
                    tooltip="Save file (Ctrl+S)"
                    type="button"
                    aria-label="Save file"
                    onClick={props.onSave}
                    disabled={!props.canEdit || !props.isDirty}
                    isLoading={props.isSaving}
                    size="sm"
                    className="h-9 w-9 rounded-md p-0 font-semibold shadow-sm shadow-blue-950/30"
                >
                    <Save className="h-4 w-4" aria-hidden="true" />
                </Button>
            )}
            {props.preview !== undefined && props.onPreviewChange ? (
                <ToggleButton
                    pressed={props.preview}
                    label="Preview"
                    tooltip={
                        props.preview
                            ? "Show the markdown editor"
                            : "Preview rendered markdown"
                    }
                    onClick={() => props.onPreviewChange?.(!props.preview)}
                >
                    <ScanText className="h-4 w-4" aria-hidden="true" />
                </ToggleButton>
            ) : null}
            <ToggleButton
                pressed={props.isSearchOpen}
                label="Toggle search and replace"
                tooltip={
                    props.isSearchOpen
                        ? "Close search and replace (Ctrl+F)"
                        : "Search and replace in the file (Ctrl+F)"
                }
                onClick={props.onToggleSearch}
                size="lg"
                className="aria-[pressed=false]:text-slate-200 aria-[pressed=false]:hover:bg-tint"
            >
                <Replace className="h-4 w-4" aria-hidden="true" />
            </ToggleButton>
            <Button
                tooltip="Copy the selection as a fenced code block headed by path#Lline, ready to reference this file in prompts to AI agents."
                type="button"
                variant="secondary"
                size="sm"
                aria-label="Copy selection with file reference"
                disabled={props.selection === null}
                isLoading={copyMutation.isPending}
                onClick={() => copyMutation.mutate()}
                className="h-9 w-9 rounded-md p-0 font-semibold"
            >
                <ClipboardCopy className="h-4 w-4" aria-hidden="true" />
            </Button>
            <Button
                tooltip="Search the selected text from the git repository root when the file is in a worktree."
                type="button"
                variant="secondary"
                size="sm"
                aria-label="Search selected text"
                disabled={props.selection === null}
                onClick={() => {
                    if (props.selection === null) {
                        return;
                    }

                    void updateSearch({
                        q: props.selection.text,
                        mode: "content",
                        gitroot: true,
                    });
                }}
                className="h-9 w-9 rounded-md p-0 font-semibold"
            >
                <Search className="h-4 w-4" aria-hidden="true" />
            </Button>
            {props.statusMessage ? (
                <span
                    role="status"
                    aria-label="File edit status"
                    aria-live="polite"
                    className={`px-2 text-sm ${
                        props.hasError
                            ? "text-red-300"
                            : props.isSaved
                              ? "text-emerald-300"
                              : "text-slate-400"
                    }`}
                >
                    {props.statusMessage}
                </span>
            ) : null}
            {copyMutation.isSuccess || copyMutation.isError ? (
                <Toast
                    tone={copyMutation.isError ? "error" : "success"}
                    icon={<ClipboardCopy className="h-4 w-4" />}
                    dismissAriaLabel="Dismiss copy reference message"
                    onDismiss={() => copyMutation.reset()}
                >
                    {copyMutation.isError
                        ? getErrorMessage(
                              copyMutation.error,
                              "Could not copy the file reference",
                          )
                        : "Copied selection with file reference"}
                </Toast>
            ) : null}
        </>
    );
}

/** Keeps file operations and editor preferences close to the surface they affect. */
function EditorActionsMenu(props: {
    agent: Agent;
    path: string;
    fileName: string;
    /** Hides file operations, which would act on the working file rather than the revision. */
    readOnly: boolean;
    canEdit: boolean;
    isReloading: boolean;
    isSaving: boolean;
    downloadUrl: string | null;
    onReload: () => void;
    onDelete: () => void;
}) {
    const [userState, setUserState] = useUserState();

    return (
        <ActionMenu
            label="Editor actions"
            title="Editor actions"
            closeAriaLabel="Close editor actions"
            hideTitle={false}
            icon={<MoreHorizontal className="h-4 w-4" />}
            variant="icon"
        >
            {(close) => (
                <>
                    {props.readOnly ? null : (
                        <>
                            <ActionMenuButton
                                disabled={!props.canEdit || props.isSaving}
                                onClick={() => {
                                    close();
                                    props.onReload();
                                }}
                            >
                                {props.isReloading ? (
                                    <LoaderCircle className="h-4 w-4 animate-spin text-slate-400" />
                                ) : (
                                    <RefreshCw className="h-4 w-4 text-slate-400" />
                                )}
                                {props.isReloading ? "Reloading..." : "Reload"}
                            </ActionMenuButton>
                        </>
                    )}
                    {props.downloadUrl === null ? null : (
                        <ActionMenuButton asChild>
                            <a
                                href={props.downloadUrl}
                                download={props.fileName}
                                onClick={close}
                            >
                                <Download className="h-4 w-4 text-slate-400" />
                                Download
                            </a>
                        </ActionMenuButton>
                    )}
                    {props.readOnly ? null : (
                        <>
                            <ActionMenuButton
                                tone="danger"
                                disabled={
                                    !props.canEdit ||
                                    props.isSaving ||
                                    getImmediateParentPath(props.path) === null
                                }
                                onClick={() => {
                                    close();
                                    props.onDelete();
                                }}
                            >
                                <Trash2 className="h-4 w-4" />
                                Delete file
                            </ActionMenuButton>
                            <div className="my-1 border-t border-slate-800" />
                            <SelectPathMenuButton
                                agent={props.agent}
                                path={props.path}
                                fileName={props.fileName}
                                entryType="file"
                                close={close}
                            />
                            <BookmarkMenuButton
                                bookmark={{
                                    agentId: props.agent.id,
                                    path: props.path,
                                    name: props.fileName,
                                    entryType: "file",
                                }}
                                close={close}
                            />
                            <div className="my-1 border-t border-slate-800" />
                        </>
                    )}
                    <Checkbox
                        checked={userState.wrapEditorLines}
                        label="Wrap lines"
                        title={false}
                        className="w-full px-3 py-2"
                        onCheckedChange={(checked) => {
                            setUserState((current) => ({
                                ...current,
                                wrapEditorLines: checked,
                            }));
                        }}
                    >
                        Wrap lines
                    </Checkbox>
                    <Checkbox
                        checked={userState.vimMode}
                        label="Vim mode"
                        title={false}
                        className="w-full px-3 py-2"
                        onCheckedChange={(checked) => {
                            setUserState((current) => ({
                                ...current,
                                vimMode: checked,
                            }));
                        }}
                    >
                        Vim mode
                    </Checkbox>
                </>
            )}
        </ActionMenu>
    );
}

/** Puts the editor exit on the rendered page so it stays with the markdown, not the toolbar. */
function CloseMarkdownPreviewButton(props: { onClose: () => void }) {
    return (
        <IconButton
            type="button"
            label="Close markdown preview"
            tooltip="Edit markdown"
            onClick={props.onClose}
            variant="ghost"
            size="lg"
            className="bg-surface/90 text-slate-300 hover:text-slate-50"
        >
            <X className="h-4 w-4" aria-hidden="true" />
        </IconButton>
    );
}

/** Groups secondary editor controls at the trailing edge of the file card. */
function FileEditorSecondaryActions(props: {
    agent: Agent;
    path: string;
    fileName: string;
    readOnly: boolean;
    canEdit: boolean;
    isReloading: boolean;
    isSaving: boolean;
    downloadUrl: string | null;
    isFullWindow: boolean;
    onToggleFullWindow: () => void;
    onReload: () => void;
    onDelete: () => void;
}) {
    const terminals = useTerminals();
    const terminalTarget = useActiveTerminalTarget();

    const [isDrawerCollapsed, setDrawerCollapsed] = useAtom(
        bottomDrawerCollapsedAtom,
    );

    const activeDrawerTab = useAtomValue(bottomDrawerActiveTabAtom);
    const isTerminalOpen = !isDrawerCollapsed && activeDrawerTab === "terminal";

    return (
        <div className="flex shrink-0 items-center gap-1">
            <IconButton
                type="button"
                label="Open terminal"
                tooltip={
                    isTerminalOpen
                        ? "Fold down terminal"
                        : "Open terminal (t, Alt+t / Option+T)"
                }
                aria-expanded={isTerminalOpen}
                onClick={() => {
                    if (isTerminalOpen) {
                        setDrawerCollapsed(true);
                    } else {
                        terminals.openOrFocus(terminalTarget);
                    }
                }}
                variant="ghost"
                size="xl"
            >
                <Terminal className="h-4 w-4" aria-hidden="true" />
            </IconButton>
            <FullWindowToggle
                targetName="editor"
                isFullWindow={props.isFullWindow}
                onToggle={props.onToggleFullWindow}
            />
            <EditorActionsMenu
                agent={props.agent}
                path={props.path}
                fileName={props.fileName}
                readOnly={props.readOnly}
                canEdit={props.canEdit}
                isReloading={props.isReloading}
                isSaving={props.isSaving}
                downloadUrl={props.downloadUrl}
                onReload={props.onReload}
                onDelete={props.onDelete}
            />
        </div>
    );
}

/**
 * The single owner of Mod-S, whether or not CodeMirror has focus, so the browser's save-page
 * dialog never opens over the editor. The capture phase runs before the editor's own handlers,
 * which is why CodeMirror needs no binding of its own. The terminal keeps its keystrokes.
 */
function useFileSaveShortcut(onSave: () => void) {
    useShortcut({
        name: "editor-save",
        phase: "capture",
        priority: shortcutPriority.editor,
        effect: "swallow",
        match: (event) =>
            (event.ctrlKey || event.metaKey) &&
            !event.altKey &&
            event.key === "s" &&
            !event.defaultPrevented &&
            !isTerminalInputTarget(event.target),
        run: onSave,
    });
}

/**
 * Warns that the device changed under unsaved browser edits. The browser version wins until
 * the user decides, so the device text is only loaded when they discard their changes;
 * keeping them dismisses the warning, and saving overwrites the device.
 */
function DeviceChangedBanner(props: {
    deviceName: string;
    onKeep: () => void;
    onDiscard: () => void;
}) {
    return (
        <div
            role="alert"
            aria-label="File changed on device"
            className="flex shrink-0 flex-wrap items-center justify-between gap-2 border-b border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-200"
        >
            <span>
                This file changed on {props.deviceName} after your unsaved edits
                started. Saving overwrites the device version.
            </span>
            <div className="flex gap-2">
                <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    tooltip="Dismiss this warning and keep editing the browser version"
                    onClick={props.onKeep}
                >
                    Keep my changes
                </Button>
                <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    tooltip="Throw away the unsaved browser changes and load the file from the device"
                    onClick={props.onDiscard}
                >
                    Discard my changes
                </Button>
            </div>
        </div>
    );
}

/** Selects the editor body without coupling its loading states to buffer ownership. */
function FileEditorSurface(props: {
    deviceName: string;
    filePath: string;
    repositoryRoot: string | null;
    isPending: boolean;
    error?: Error;
    bufferKey: string;
    deviceContent: string;
    stored: StoredBuffer | null;
    previewContent: string;
    fileName: string;
    editable: boolean;
    vimMode: boolean;
    wrapLines: boolean;
    preview: boolean;
    scrollToLine?: number;
    reveal?: OpenFileReveal;
    searchHandleRef: React.RefObject<EditorSearchHandle | null>;
    isSearchOpen: boolean;
    onSearchOpenChange: (open: boolean) => void;
    onBecameDirty: () => void;
    onDoubleClick?: () => void;
    onFocus: () => void;
    onSave: () => void;
    onSelectionChange: (selection: EditorSelection | null) => void;
    onClosePreview: () => void;
}) {
    if (props.isPending) {
        return <p className="p-4 text-sm text-slate-400">Loading file...</p>;
    }

    if (props.error !== undefined) {
        return (
            <p className="p-4 text-sm text-red-300">
                {getErrorMessage(props.error, "Failed to load file")}
            </p>
        );
    }

    return (
        <>
            <div
                aria-hidden={props.preview}
                className={props.preview ? "hidden" : "flex min-h-0 flex-1"}
            >
                <CodeEditor
                    bufferKey={props.bufferKey}
                    deviceContent={props.deviceContent}
                    stored={props.stored}
                    fileName={props.fileName}
                    editable={props.editable}
                    vimMode={props.vimMode}
                    wrapLines={props.wrapLines}
                    scrollToLine={props.scrollToLine}
                    reveal={props.reveal}
                    onBecameDirty={props.onBecameDirty}
                    onDoubleClick={props.onDoubleClick}
                    onFocus={props.onFocus}
                    onSave={props.onSave}
                    onSelectionChange={props.onSelectionChange}
                    searchHandleRef={props.searchHandleRef}
                    isSearchOpen={props.isSearchOpen}
                    onSearchOpenChange={props.onSearchOpenChange}
                    captureFindKeys={!props.preview}
                />
            </div>
            <div
                aria-hidden={!props.preview}
                className={
                    props.preview ? "relative flex min-h-0 flex-1" : "hidden"
                }
            >
                <MarkdownPreview
                    content={props.previewContent}
                    deviceName={props.deviceName}
                    filePath={props.filePath}
                    repositoryRoot={props.repositoryRoot}
                />
                <div className="absolute top-0 right-1 z-10">
                    <CloseMarkdownPreviewButton
                        onClose={props.onClosePreview}
                    />
                </div>
            </div>
        </>
    );
}

/** Chooses the single editor status announced beside file actions. */
function getFileEditorStatus(props: {
    readOnly: boolean;
    isLoading: boolean;
    loadError?: Error;
    isSaving: boolean;
    isSaved: boolean;
    saveError?: Error;
    isDirty: boolean;
}): string | null {
    if (props.isLoading) return "Loading file...";

    if (props.loadError) {
        return getErrorMessage(props.loadError, "Failed to load file");
    }

    if (props.isSaving) return "Saving...";

    if (props.isSaved) return "Saved";

    if (props.saveError) {
        return getErrorMessage(props.saveError, "Failed to save file");
    }

    if (props.readOnly) return "Read-only revision";

    return props.isDirty ? "Unsaved changes" : null;
}

/** Makes file removal confirm draft loss for both trash and permanent deletion. */
function EditorDeleteDialog(props: {
    agent: Agent;
    filePath: string;
    fileName: string;
    isDirty: boolean;
    onClose: () => void;
}) {
    return (
        <DeletePathDialog
            agent={props.agent}
            path={props.filePath}
            name={props.fileName}
            entryType="file"
            leaveToParent
            onClose={props.onClose}
        >
            {props.isDirty ? (
                <p className="mt-3 text-sm text-amber-300">
                    Unsaved edits will be discarded if you remove this file.
                </p>
            ) : null}
        </DeletePathDialog>
    );
}

/**
 * Keeps editor route context explicit because previews need filesystem and repository paths.
 */
type FileEditViewProps = {
    agent: Agent;
    fileName: string;
    filePath: string;
    /**
     * Shows the file as committed at this Git revision instead of the working file. The
     * editor is then read-only and offers no save, reload or file operations.
     */
    revision?: string;
    repositoryRoot: string | null;
    mimeType: string;
    /** Absent when the shown content has no raw download, such as a Git revision. */
    downloadUrl: string | null;
    scrollToLine?: number;
    preview: boolean;
    onPreviewChange: (preview: boolean) => void;
    isFullWindow: boolean;
    onToggleFullWindow: () => void;
};

/** Groups editor toolbar actions so the edit view can stay focused on buffer state. */
function FileEditHeader(props: {
    readOnly: boolean;
    fileName: string;
    statusMessage: string | null;
    hasError: boolean;
    isSaved: boolean;
    canEdit: boolean;
    isDirty: boolean;
    isSaving: boolean;
    agent: Agent;
    filePath: string;
    selection: EditorSelection | null;
    preview: boolean | undefined;
    isSearchOpen: boolean;
    isReloading: boolean;
    downloadUrl: string | null;
    isFullWindow: boolean;
    onSave: () => void;
    onToggleSearch: () => void;
    onPreviewChange: ((preview: boolean) => void) | undefined;
    onToggleFullWindow: () => void;
    onReload: () => void;
    onDelete: () => void;
}) {
    return (
        <header className="shrink-0 border-b border-slate-800 p-3">
            <h1 aria-label="File name" className="sr-only">
                {props.fileName}
            </h1>
            <div className="flex flex-wrap items-center justify-between gap-1">
                <div className="flex flex-wrap items-center justify-start gap-2">
                    <FileEditActions
                        readOnly={props.readOnly}
                        statusMessage={props.statusMessage}
                        hasError={props.hasError}
                        isSaved={props.isSaved}
                        canEdit={props.canEdit}
                        isDirty={props.isDirty}
                        isSaving={props.isSaving}
                        bookmark={{
                            agentId: props.agent.id,
                            path: props.filePath,
                            name: props.fileName,
                            entryType: "file",
                        }}
                        selection={props.selection}
                        preview={props.preview}
                        isSearchOpen={props.isSearchOpen}
                        onSave={props.onSave}
                        onToggleSearch={props.onToggleSearch}
                        onPreviewChange={props.onPreviewChange}
                    />
                </div>
                <FileEditorSecondaryActions
                    agent={props.agent}
                    path={props.filePath}
                    fileName={props.fileName}
                    readOnly={props.readOnly}
                    canEdit={props.canEdit}
                    isReloading={props.isReloading}
                    isSaving={props.isSaving}
                    downloadUrl={props.downloadUrl}
                    isFullWindow={props.isFullWindow}
                    onToggleFullWindow={props.onToggleFullWindow}
                    onReload={props.onReload}
                    onDelete={props.onDelete}
                />
            </div>
        </header>
    );
}

/**
 * Joins a file's device text, its server copy and its buffer in the editor workspace. The
 * buffer's document is the draft: dirty tracking and saving read it, so there is no second
 * copy of the text in React state.
 */
function useFileBuffer(props: FileEditViewProps) {
    const { api, editor } = rootRouteApi.useRouteContext();
    const queryClient = useQueryClient();
    const [userState] = useUserState();
    const readOnly = props.revision !== undefined;
    const fileQuery = fileContentQueryOptions(props.agent, props.filePath);
    const contentQuery = useEditorContentQuery(props);

    // A revision is a read-only preview of other text, so it never shares the file's buffer.
    const bufferKey = readOnly
        ? `revision:${props.revision ?? ""}:${props.agent.id}:${props.filePath}`
        : getOpenFileKey({ agentId: props.agent.id, path: props.filePath });

    const storedQuery = useQuery({
        ...editorBufferQueryOptions(api, bufferKey),
        enabled: !readOnly,
    });

    const statuses = React.useSyncExternalStore(
        editor.subscribe,
        editor.getSnapshot,
    );

    const status = statuses.get(bufferKey);
    const openFiles = userState.openFiles;

    // A revision's key never matches an open file, so it never takes the file's line request.
    const reveal = openFiles.find(
        (file) => getOpenFileKey(file) === bufferKey,
    )?.reveal;

    // Tells the workspace which buffers to keep and write, before the editor activates.
    React.useLayoutEffect(() => {
        editor.setOpenKeys(openFiles.map((file) => getOpenFileKey(file)));
    }, [editor, openFiles]);

    const saveMutation = useMutation({
        mutationFn: async (nextContent: string) => {
            await props.agent.editFile(
                props.filePath,
                new globalThis.File([nextContent], props.fileName, {
                    type: props.mimeType || "text/plain",
                }),
            );

            return nextContent;
        },
        onSuccess: async (nextContent) => {
            // A read that started before the write would bring back the old text as if the
            // device had changed it, and a clean buffer would follow it.
            await queryClient.cancelQueries({ queryKey: fileQuery.queryKey });
            // The buffer learns its new base before the cache update reaches it as device text.
            editor.rebase(bufferKey, nextContent);
            queryClient.setQueryData(fileQuery.queryKey, nextContent);
        },
    });

    return {
        editor,
        readOnly,
        bufferKey,
        reveal,
        contentQuery,
        storedQuery,
        saveMutation,
        isDirty: status?.isDirty === true,
        conflict: status?.conflict ?? null,
        // A failed server read must not keep the file from opening from the device.
        isStoredReady: readOnly || !storedQuery.isPending,
    };
}

/**
 * Edits file contents in a viewport-bounded CodeMirror with explicit save and reload.
 * scrollToLine is inbound-only so a ?line= URL can move the caret without writing back.
 * There is no unsaved-changes prompt on leaving: an edited file is open, and its buffer is
 * kept on the server until the file is closed from the open files list.
 */
export function FileEditView(props: FileEditViewProps) {
    const [deleteConfirmationOpen, setDeleteConfirmationOpen] =
        React.useState(false);

    const [userState] = useUserState();
    const buffer = useFileBuffer(props);
    const contentQuery = buffer.contentQuery;
    const saveMutation = buffer.saveMutation;
    const isDirty = buffer.isDirty;

    const [selection, setSelection] = React.useState<EditorSelection | null>(
        null,
    );

    const [isSearchOpen, setIsSearchOpen] = React.useState(false);
    const searchHandleRef = React.useRef<EditorSearchHandle | null>(null);
    const reloadPromiseRef = React.useRef<Promise<unknown> | null>(null);
    const isSavingRef = React.useRef(false);

    // Viewing a file does not open it; the first change away from the device text or a double
    // click in the editor does.
    const keepOpen = useKeepFileOpen({
        file: {
            agentId: props.agent.id,
            path: props.filePath,
            name: props.fileName,
            agentName: props.agent.name,
        },
    });

    // Saving must not flip CodeMirror read-only, or Mod-s and :w steal editor focus.
    const canEdit = contentQuery.isSuccess && !buffer.readOnly;

    const isMarkdown =
        syntaxLanguageFromFileName(props.fileName) === "markdown";

    const isPreviewShown = isMarkdown && props.preview;

    const previewContent = React.useSyncExternalStore(
        buffer.editor.subscribeDocument,
        () => (isPreviewShown ? buffer.editor.text(buffer.bufferKey) : ""),
    );

    useEditorRefreshRegistration({
        agentId: props.agent.id,
        path: props.filePath,
    });

    // Fetches the device text and the server copy. Neither replaces unsaved edits: a dirty
    // buffer only reports a conflict, and a newer server copy is adopted only when clean.
    const reloadFile = React.useCallback(() => {
        // `:w` refocuses the editor in the same tick it starts saving, before isPending renders.
        if (
            !canEdit ||
            saveMutation.isPending ||
            isSavingRef.current ||
            reloadPromiseRef.current
        ) {
            return;
        }

        const reloadPromise = Promise.all([
            contentQuery.refetch({ cancelRefetch: false }),
            buffer.storedQuery.refetch({ cancelRefetch: false }),
        ]).finally(() => {
            reloadPromiseRef.current = null;
        });

        reloadPromiseRef.current = reloadPromise;
    }, [buffer.storedQuery, canEdit, contentQuery, saveMutation.isPending]);

    const handleSave = React.useCallback(() => {
        if (!canEdit || saveMutation.isPending || !isDirty) {
            return;
        }

        isSavingRef.current = true;
        saveMutation.mutate(buffer.editor.text(buffer.bufferKey), {
            onSettled: () => {
                isSavingRef.current = false;
            },
        });
    }, [buffer.bufferKey, buffer.editor, canEdit, isDirty, saveMutation]);

    useFileSaveShortcut(handleSave);

    const statusMessage = getFileEditorStatus({
        readOnly: buffer.readOnly,
        isLoading: contentQuery.isPending,
        loadError: contentQuery.isError ? contentQuery.error : undefined,
        isSaving: saveMutation.isPending,
        isSaved: saveMutation.isSuccess && !isDirty,
        saveError: saveMutation.isError ? saveMutation.error : undefined,
        isDirty,
    });

    return (
        <div className="flex min-h-0 flex-1 flex-col">
            <article
                aria-label="Editing panel"
                data-editor-full-window={props.isFullWindow}
                className={`flex min-h-0 flex-1 flex-col overflow-hidden bg-surface shadow-2xl shadow-black/20 ${
                    props.isFullWindow ? "fixed inset-0 z-[60]" : ""
                }`}
            >
                {props.isFullWindow ? (
                    <EditorOpenFiles
                        agentId={props.agent.id}
                        filePath={props.filePath}
                    />
                ) : null}
                <FileEditHeader
                    readOnly={buffer.readOnly}
                    fileName={props.fileName}
                    statusMessage={statusMessage}
                    hasError={contentQuery.isError || saveMutation.isError}
                    isSaved={saveMutation.isSuccess && !isDirty}
                    canEdit={canEdit}
                    isDirty={isDirty}
                    isSaving={saveMutation.isPending}
                    agent={props.agent}
                    filePath={props.filePath}
                    selection={selection}
                    preview={isMarkdown ? props.preview : undefined}
                    isSearchOpen={isSearchOpen}
                    isReloading={contentQuery.isFetching}
                    downloadUrl={props.downloadUrl}
                    isFullWindow={props.isFullWindow}
                    onSave={handleSave}
                    onToggleSearch={() => {
                        if (!searchHandleRef.current?.close()) {
                            searchHandleRef.current?.open();
                        }
                    }}
                    onPreviewChange={
                        isMarkdown ? props.onPreviewChange : undefined
                    }
                    onToggleFullWindow={props.onToggleFullWindow}
                    onReload={reloadFile}
                    onDelete={() => setDeleteConfirmationOpen(true)}
                />
                {buffer.conflict === null ? null : (
                    <DeviceChangedBanner
                        deviceName={props.agent.name}
                        onKeep={() => {
                            if (buffer.conflict !== null) {
                                buffer.editor.rebase(
                                    buffer.bufferKey,
                                    buffer.conflict,
                                );
                            }
                        }}
                        onDiscard={() => {
                            if (buffer.conflict !== null) {
                                buffer.editor.discard(
                                    buffer.bufferKey,
                                    buffer.conflict,
                                );
                            }
                        }}
                    />
                )}

                <div className="flex min-h-0 flex-1 flex-col">
                    <FileEditorSurface
                        deviceName={props.agent.name}
                        filePath={props.filePath}
                        repositoryRoot={props.repositoryRoot}
                        isPending={
                            contentQuery.isPending || !buffer.isStoredReady
                        }
                        error={
                            contentQuery.isError
                                ? contentQuery.error
                                : undefined
                        }
                        bufferKey={buffer.bufferKey}
                        deviceContent={contentQuery.data ?? ""}
                        stored={buffer.storedQuery.data ?? null}
                        previewContent={previewContent}
                        fileName={props.fileName}
                        editable={canEdit}
                        vimMode={userState.vimMode}
                        wrapLines={userState.wrapEditorLines}
                        preview={isPreviewShown}
                        scrollToLine={props.scrollToLine}
                        reveal={buffer.reveal}
                        onBecameDirty={() => {
                            if (saveMutation.isSuccess) saveMutation.reset();

                            if (!buffer.readOnly) {
                                keepOpen();
                            }
                        }}
                        // A revision is not the working file, so there is nothing to open.
                        onDoubleClick={buffer.readOnly ? undefined : keepOpen}
                        onFocus={reloadFile}
                        onSave={handleSave}
                        onSelectionChange={setSelection}
                        searchHandleRef={searchHandleRef}
                        isSearchOpen={isSearchOpen}
                        onSearchOpenChange={setIsSearchOpen}
                        onClosePreview={() => props.onPreviewChange(false)}
                    />
                </div>
            </article>
            {deleteConfirmationOpen ? (
                <EditorDeleteDialog
                    agent={props.agent}
                    filePath={props.filePath}
                    fileName={props.fileName}
                    isDirty={isDirty}
                    onClose={() => setDeleteConfirmationOpen(false)}
                />
            ) : null}
        </div>
    );
}

/**
 * Reads the text the editor shows: the working file, or the committed file when a revision
 * is browsed. A revision has its own query key, so it never shares a buffer with the
 * working file, and only the query for the shown source runs.
 */
function useEditorContentQuery(props: {
    agent: Agent;
    filePath: string;
    revision?: string;
}) {
    const readOnly = props.revision !== undefined;

    const worktreeContentQuery = useQuery({
        ...fileContentQueryOptions(props.agent, props.filePath),
        enabled: !readOnly,
    });

    const revisionContentQuery = useQuery({
        ...gitRevisionContentQueryOptions(
            props.agent,
            props.filePath,
            props.revision ?? "",
        ),
        enabled: readOnly,
    });

    return readOnly ? revisionContentQuery : worktreeContentQuery;
}

/** Renders agent-verified images through the authenticated raw download URL. */
export function FileImageView(props: {
    agent: Agent;
    path: string;
    fileName: string;
    downloadUrl: string;
}) {
    return (
        <BrowserViewCard>
            <ViewCardHeader
                icon={<ImageIcon className="h-5 w-5" aria-hidden="true" />}
                eyebrow="Image"
                title={props.fileName}
                titleAriaLabel="File name"
                actions={
                    <PersistentPathActions
                        agent={props.agent}
                        path={props.path}
                        currentName={props.fileName}
                        entryType="file"
                        downloadUrl={props.downloadUrl}
                        downloadName={props.fileName}
                    />
                }
            />

            <div className="flex items-center justify-center p-4 @2xl:p-6">
                <img
                    src={props.downloadUrl}
                    alt={props.fileName}
                    className="max-h-[70vh] max-w-full rounded-xl object-contain"
                />
            </div>
        </BrowserViewCard>
    );
}
