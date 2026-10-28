import React from "react";
import { Link } from "@tanstack/react-router";
import { FileText } from "lucide-react";
import { browserRoute } from "#ui/browser-route";
import { isExpandable, parseDiff, type DiffFile } from "#ui/utils/diff-parse";
import {
    Diff,
    diffFileName,
    type DiffEditorRoute,
} from "#ui/components/browser/diff-view";
import { WholeFileToggle } from "#ui/components/browser/diff-layout-control";
import {
    OpenInBackgroundButton,
    useBackgroundOpen,
    type BackgroundOpen,
} from "#ui/components/browser/open-file-actions";
import { getBaseName } from "#ui/utils/path";

/** Words for the file-level changes worth a badge; plain modifications need none. */
const FILE_TYPE_LABELS = {
    add: "Added",
    delete: "Deleted",
    modify: null,
    rename: "Renamed",
    copy: "Copied",
} satisfies Record<DiffFile["type"], string | null>;

/** Counts added and removed lines, a modified row counting as one of each, like `git diff --stat`. */
function changeCounts(file: DiffFile) {
    let added = 0;
    let removed = 0;

    for (const hunk of file.hunks) {
        if (hunk.type !== "hunk") {
            continue;
        }

        for (const line of hunk.lines) {
            if (line.kind === "insert" || line.kind === "modified") {
                added++;
            }

            if (line.kind === "delete" || line.kind === "modified") {
                removed++;
            }
        }
    }

    return { added, removed };
}

/**
 * Names the file above its rows. The name opens the file in the editor when there is one, so
 * the diff needs no separate heading. Views that already name the file, such as the diff
 * list's sticky headers, hide this through its data attribute.
 */
function DiffFileHeader(props: {
    file: DiffFile;
    editorRoute: DiffEditorRoute | null;
    background: BackgroundOpen | null;
    /** Null when the patch cannot show the file whole, so no toggle is offered. */
    wholeFile: { shown: boolean; onChange: (shown: boolean) => void } | null;
}) {
    const file = props.file;
    const name = diffFileName(file);
    const counts = changeCounts(file);
    const typeLabel = file.isBinary ? "Binary" : FILE_TYPE_LABELS[file.type];

    // Sync compares two different files, so both names matter, like a rename.
    const fromPath =
        file.oldPath !== null &&
        file.newPath !== null &&
        file.oldPath !== file.newPath
            ? file.oldPath
            : null;

    return (
        <div
            data-diff-file-header=""
            className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-slate-800 bg-surface-raised px-3 py-2 text-sm"
        >
            <FileText
                className="h-4 w-4 shrink-0 text-slate-400"
                aria-hidden="true"
            />
            {fromPath === null ? null : (
                <span className="min-w-0 break-all font-mono text-xs text-slate-400">
                    {fromPath} →
                </span>
            )}
            {props.editorRoute === null ? (
                <span className="min-w-0 break-all font-mono text-xs text-slate-200">
                    {name}
                </span>
            ) : (
                <Link
                    {...props.editorRoute}
                    search={{}}
                    className="min-w-0 break-all font-mono text-xs text-blue-300 hover:underline focus-visible:underline"
                >
                    {name}
                </Link>
            )}
            {props.background === null ? null : (
                <OpenInBackgroundButton target={props.background} />
            )}
            {typeLabel === null ? null : (
                <span className="rounded-full border border-slate-700 px-2 py-0.5 text-xs text-slate-400">
                    {typeLabel}
                </span>
            )}
            <span className="ml-auto flex items-center gap-3">
                {counts.added + counts.removed === 0 ? null : (
                    <span className="font-mono text-xs tabular-nums">
                        <span className="text-emerald-400">
                            +{counts.added}
                        </span>{" "}
                        <span className="text-red-400">−{counts.removed}</span>
                    </span>
                )}
                {props.wholeFile === null ? null : (
                    <WholeFileToggle
                        label={`Show whole file ${name}`}
                        shown={props.wholeFile.shown}
                        onChange={props.wholeFile.onChange}
                        className="h-7 w-7"
                    />
                )}
            </span>
        </div>
    );
}

/** Explains a file without rows, so it never reads as an empty or broken diff. */
function emptyFileMessage(file: DiffFile): string {
    if (file.isBinary) {
        return "Binary file not shown.";
    }

    if (file.type === "rename" || file.type === "copy") {
        return "The content did not change.";
    }

    return "No content changes.";
}

/** Turns trusted server-generated unified diff text into the shared accessible table. */
export function UnifiedDiff(props: {
    unifiedDiff: string;
    emptyMessage?: string;
    /**
     * The current file, so the file name and trailing Open links can jump to it with
     * ?line=, or open it without leaving the diff. The device's current name is what the
     * browser URL holds; its id is what the open files list keys the file by.
     */
    editorFile?: { device: { id: string; name: string }; path: string };
    /**
     * Whether a similar removed and added line show as one row with inline changes. It
     * comes from the reader's stored preference, which the diff views own.
     */
    mergeModifiedLines?: boolean;
    /**
     * Whether long lines wrap inside the view instead of scrolling it sideways. It comes from
     * the reader's stored preference, which the diff views own.
     */
    wrapLines?: boolean;
    /**
     * Whether the patch carries every unchanged line, as the server says, so the view folds
     * them itself and can reveal them without fetching.
     */
    fullContext?: boolean;
    /**
     * Shows every file whole. Diff lists set it from their own header, which replaces the
     * file header here; other views leave it unset and toggle each file in its header.
     */
    wholeFile?: boolean;
}) {
    const mergeModifiedLines = props.mergeModifiedLines ?? true;
    const fullContext = props.fullContext ?? false;

    // Files are toggled by position, since a patch's files have no other stable identity.
    const [wholeFiles, setWholeFiles] = React.useState<ReadonlySet<number>>(
        () => new Set(),
    );

    const showWholeFile = (index: number, shown: boolean) =>
        setWholeFiles((current) => {
            const next = new Set(current);

            if (shown) {
                next.add(index);
            } else {
                next.delete(index);
            }

            return next;
        });

    const editorDeviceId = props.editorFile?.device.id;
    const editorDeviceName = props.editorFile?.device.name;
    const editorPath = props.editorFile?.path;

    // Parsing pairs similar lines with word diffs, which is too slow to redo every render.
    const files = React.useMemo(
        () => parseDiff(props.unifiedDiff, { mergeModifiedLines, fullContext }),
        [props.unifiedDiff, mergeModifiedLines, fullContext],
    );

    // A stable route and file keep the memoized rows from re-rendering when the parent does.
    const editorRoute = React.useMemo(
        () =>
            editorDeviceId === undefined ||
            editorDeviceName === undefined ||
            editorPath === undefined
                ? null
                : browserRoute(
                      { id: editorDeviceId, name: editorDeviceName },
                      editorPath,
                  ),
        [editorDeviceId, editorDeviceName, editorPath],
    );

    const backgroundFile = React.useMemo(
        () =>
            editorDeviceId === undefined ||
            editorDeviceName === undefined ||
            editorPath === undefined
                ? null
                : {
                      agentId: editorDeviceId,
                      agentName: editorDeviceName,
                      path: editorPath,
                      name: getBaseName(editorPath),
                  },
        [editorDeviceId, editorDeviceName, editorPath],
    );

    const background = useBackgroundOpen(backgroundFile);

    if (props.unifiedDiff === "") {
        return (
            <p className="text-sm text-slate-400">
                {props.emptyMessage ?? "The files are identical."}
            </p>
        );
    }

    if (files.length === 0) {
        // Text the reader does not recognise as a patch is still worth showing as is.
        return (
            <pre className="whitespace-pre p-3 font-mono text-xs text-slate-200">
                {props.unifiedDiff}
            </pre>
        );
    }

    return (
        <div className="grid w-full min-w-0 grid-cols-[minmax(0,1fr)]">
            {files.map((file, index) => {
                const name = diffFileName(file);
                const hasRows = file.hunks.some((hunk) => hunk.type === "hunk");
                const wholeFile = props.wholeFile ?? wholeFiles.has(index);

                return (
                    <div key={index} className="min-w-0">
                        <DiffFileHeader
                            file={file}
                            editorRoute={editorRoute}
                            background={background}
                            wholeFile={
                                props.wholeFile === undefined &&
                                isExpandable(file)
                                    ? {
                                          shown: wholeFile,
                                          onChange: (shown) =>
                                              showWholeFile(index, shown),
                                      }
                                    : null
                            }
                        />
                        {hasRows ? (
                            <Diff
                                file={file}
                                label={
                                    name === ""
                                        ? "Changes"
                                        : `Changes in ${name}`
                                }
                                editorRoute={editorRoute}
                                background={background}
                                wrapLines={props.wrapLines ?? false}
                                wholeFile={wholeFile}
                            />
                        ) : (
                            <p className="px-3 py-2 text-sm text-slate-400">
                                {emptyFileMessage(file)}
                            </p>
                        )}
                    </div>
                );
            })}
        </div>
    );
}
