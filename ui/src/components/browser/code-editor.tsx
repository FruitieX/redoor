import React from "react";
import CodeMirror, { type ReactCodeMirrorRef } from "@uiw/react-codemirror";
import { bbedit } from "@uiw/codemirror-theme-bbedit";
import { tokyoNight } from "@uiw/codemirror-theme-tokyo-night";
import { search } from "@codemirror/search";
import { Facet, type EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { vim, Vim } from "@replit/codemirror-vim";
import {
    EditorSearch,
    type EditorSearchHandle,
} from "#ui/components/browser/editor-search";
import { vimYankHighlightExtension } from "./vim-yank-highlight";
import { languageFromFileName } from "#ui/utils/editor-language";
import { shortcutPriority, useShortcut } from "#ui/shortcuts";
import { useTerminals } from "#ui/use-terminals";
import { isTerminalInputTarget, isUnmodifiedAltKey } from "#ui/utils/keyboard";
import { getBaseName } from "#ui/utils/path";
import { useResolvedTheme } from "#ui/utils/use-resolved-theme";

/** Carries only the editor selection details needed by actions outside CodeMirror. */
export type EditorSelection = {
    text: string;
    startLine: number;
};

/**
 * Carries each editor's save handler in its own CodeMirror state. Vim's `:w` is registered
 * once for the whole page, so it finds the handler of the editor that ran it through that
 * editor's state, rather than through a shared variable that assumes only one editor exists.
 */
const vimWriteHandler = Facet.define<() => void, (() => void) | null>({
    combine: (handlers) => handlers[0] ?? null,
});

Vim.defineEx("write", "w", (cm) => {
    cm.cm6.state.facet(vimWriteHandler)?.();
});

/**
 * Reclaims editor focus from an active shell with Alt-e, and drops a pending delayed
 * correction when a newer terminal activation arrives, which must win.
 */
function useFocusEditorShortcut(view: EditorView | null) {
    const terminals = useTerminals();
    const focusCorrectionTimer = React.useRef<number | undefined>(undefined);

    React.useEffect(() => {
        return () => window.clearTimeout(focusCorrectionTimer.current);
    }, []);

    // A watcher in the observer tier, so it still sees `t` after the terminal shortcut claims it.
    useShortcut(
        {
            name: "editor-cancel-focus-correction",
            phase: "capture",
            priority: shortcutPriority.observer,
            match: (event) =>
                isUnmodifiedAltKey(event, "t") || event.key === "t",
            run: () => {
                window.clearTimeout(focusCorrectionTimer.current);

                return false;
            },
        },
        view !== null,
    );

    useShortcut(
        {
            name: "editor-reclaim-focus",
            phase: "capture",
            priority: shortcutPriority.editor,
            match: (event) => isUnmodifiedAltKey(event, "e"),
            run: (event) => {
                // Whatever the key's target, the user went back to the editor, so a terminal
                // that finishes connecting later must not take focus again.
                terminals.cancelPendingFocus();

                if (view === null || !isTerminalInputTarget(event.target)) {
                    return false;
                }

                view.focus();
                window.clearTimeout(focusCorrectionTimer.current);

                focusCorrectionTimer.current = window.setTimeout(() => {
                    // The terminal may settle focus asynchronously, so reclaim it only if that won.
                    if (isTerminalInputTarget(document.activeElement)) {
                        view.focus();
                    }
                }, 0);
            },
        },
        view !== null,
    );
}

/** Selects the inbound ?line= target and centers it so the opened location is obvious in a tall buffer. */
function moveCaretToLine(view: EditorView, line: number) {
    const lineCount = view.state.doc.lines;

    // Empty documents have no real line to reveal; clamping would still dispatch a no-op caret.
    if (view.state.doc.length === 0 || lineCount < 1) {
        return;
    }

    const clamped = Math.min(Math.max(line, 1), lineCount);
    const docLine = view.state.doc.line(clamped);
    view.dispatch({
        selection: { anchor: docLine.from, head: docLine.to },
        effects: EditorView.scrollIntoView(docLine.from, { y: "center" }),
    });
}

/** Reads the current range so chrome and tests can follow the editor selection. */
function editorSelectionFromState(state: EditorState): EditorSelection | null {
    const selection = state.selection.main;

    if (selection.empty) {
        return null;
    }

    return {
        text: state.sliceDoc(selection.from, selection.to),
        startLine: state.doc.lineAt(selection.from).number,
    };
}

/** Applies inbound ?line= once the view exists so typing and saves do not re-scroll. */
function useInboundScrollToLine(
    view: EditorView | null,
    scrollToLine: number | undefined,
) {
    React.useLayoutEffect(() => {
        if (view === null || scrollToLine === undefined) {
            return;
        }

        moveCaretToLine(view, scrollToLine);
    }, [view, scrollToLine]);
}

/**
 * Presentational CodeMirror surface so FileEditView can keep query/draft ownership.
 * A bounded height lets CodeMirror virtualize the viewport instead of growing the page.
 * Extensions are memoized on the file name and keymap because recreating them remounts the editor.
 */
export function CodeEditor(props: {
    value: string;
    fileName: string;
    editable: boolean;
    vimMode: boolean;
    wrapLines: boolean;
    scrollToLine?: number;
    onChange: (value: string) => void;
    onFocus: () => void;
    onSave: () => void;
    onSelectionChange: (selection: EditorSelection | null) => void;
    searchHandleRef: React.RefObject<EditorSearchHandle | null>;
    isSearchOpen: boolean;
    onSearchOpenChange: (open: boolean) => void;
    /** Leaves find keys to the browser while the editor is hidden behind a preview. */
    captureFindKeys: boolean;
}) {
    const resolvedTheme = useResolvedTheme();
    const onSaveRef = React.useRef(props.onSave);
    onSaveRef.current = props.onSave;
    const editorRef = React.useRef<ReactCodeMirrorRef>(null);

    /** Saves without leaving CodeMirror so :w cannot dump the user into chrome. */
    const saveFromEditor = React.useCallback(() => {
        onSaveRef.current();
        editorRef.current?.view?.focus();
    }, []);

    const onFocusRef = React.useRef(props.onFocus);
    onFocusRef.current = props.onFocus;
    const hasReceivedFocusRef = React.useRef(false);
    const [view, setView] = React.useState<EditorView | null>(null);
    const [documentRevision, setDocumentRevision] = React.useState(0);
    const [caretLine, setCaretLine] = React.useState<number | null>(null);
    const [selectedText, setSelectedText] = React.useState<string | null>(null);
    const firstLineEnd = props.value.indexOf("\n");

    const firstLine = props.value.slice(
        0,
        firstLineEnd === -1 ? props.value.length : firstLineEnd,
    );

    const baseName = getBaseName(props.fileName);

    const languageContent =
        baseName.lastIndexOf(".") <= 0 && firstLine.startsWith("#!")
            ? firstLine
            : "";

    const extensions = React.useMemo(() => {
        const language = languageFromFileName(props.fileName, languageContent);

        return [
            // Vim must precede both its observer and other keymaps so its adapter exists and normal-mode keys win.
            ...(props.vimMode
                ? [
                      vim({ status: true }),
                      vimYankHighlightExtension,
                      vimWriteHandler.of(saveFromEditor),
                  ]
                : []),
            ...(props.wrapLines ? [EditorView.lineWrapping] : []),
            search(),
            EditorView.contentAttributes.of({
                "aria-label": "File editor",
                "data-file-editor": "",
                "data-vim-mode": props.vimMode ? "true" : "false",
                "data-wrap-lines": props.wrapLines ? "true" : "false",
            }),
            EditorView.domEventHandlers({
                focus: () => {
                    // The initial load is already fresh, so only later focus entries refetch.
                    if (hasReceivedFocusRef.current) {
                        onFocusRef.current();
                    } else {
                        hasReceivedFocusRef.current = true;
                    }
                },
            }),
            EditorView.theme({
                "&": {
                    height: "100%",
                },
                "&.cm-focused": {
                    outline: "none",
                },
                ".cm-scroller": {
                    overflow: "auto",
                },
                ".cm-content": {
                    padding: "0",
                },
                ".cm-line": {
                    padding: "0",
                },
            }),
            ...(language === undefined ? [] : [language]),
        ];
    }, [
        languageContent,
        props.fileName,
        props.vimMode,
        props.wrapLines,
        saveFromEditor,
    ]);

    useFocusEditorShortcut(view);
    useInboundScrollToLine(view, props.scrollToLine);

    return (
        <div className="flex min-h-0 flex-1 flex-col overflow-hidden">
            <EditorSearch
                view={view}
                editable={props.editable}
                documentRevision={documentRevision}
                handleRef={props.searchHandleRef}
                open={props.isSearchOpen}
                onOpenChange={props.onSearchOpenChange}
                captureFindKeys={props.captureFindKeys}
            />
            <div
                data-file-editor=""
                data-vim-mode={props.vimMode ? "true" : "false"}
                data-wrap-lines={props.wrapLines ? "true" : "false"}
                role="region"
                aria-label="Editor viewport"
                className="relative min-h-0 flex-1 overflow-hidden after:pointer-events-none after:absolute after:inset-0 after:z-20 after:border after:border-transparent focus-within:after:border-blue-500"
            >
                {caretLine === null ? null : (
                    <span className="sr-only" aria-label="Editor caret line">
                        {String(caretLine)}
                    </span>
                )}
                {selectedText === null ? null : (
                    <span className="sr-only" aria-label="Editor selected text">
                        {selectedText}
                    </span>
                )}
                <CodeMirror
                    ref={editorRef}
                    value={props.value}
                    height="100%"
                    width="100%"
                    theme={resolvedTheme === "light" ? bbedit : tokyoNight}
                    editable={props.editable}
                    basicSetup={{ searchKeymap: false }}
                    extensions={extensions}
                    onCreateEditor={(editorView) => {
                        setView(editorView);
                        setCaretLine(
                            editorView.state.doc.lineAt(
                                editorView.state.selection.main.head,
                            ).number,
                        );
                    }}
                    onChange={props.onChange}
                    onUpdate={(update) => {
                        if (update.docChanged || update.selectionSet) {
                            setDocumentRevision((revision) => revision + 1);
                            setCaretLine(
                                update.state.doc.lineAt(
                                    update.state.selection.main.head,
                                ).number,
                            );

                            const nextSelection = editorSelectionFromState(
                                update.state,
                            );

                            setSelectedText(nextSelection?.text ?? null);
                            props.onSelectionChange(nextSelection);
                        }
                    }}
                    className="h-full min-h-0"
                />
            </div>
        </div>
    );
}
