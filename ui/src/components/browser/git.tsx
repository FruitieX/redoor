import React from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
    AlertTriangle,
    CheckCircle2,
    FileDiff,
    ListChecks,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { Button } from "#ui/components/button";
import { SegmentedControl } from "#ui/components/segmented-control";
import { browserRoute } from "#ui/browser-route";
import { UnifiedDiff } from "#ui/components/browser/unified-diff";
import {
    ChangeBadge,
    GitPanelBar,
    type GitChangeKind,
} from "#ui/components/browser/git-parts";
import { gitDiffQueryOptions, gitStatusQueryOptions } from "#ui/queries";
import { getErrorMessage } from "#ui/utils/error";
import { getBaseName } from "#ui/utils/path";
import type { GitContextResponse } from "#bindings/GitContextResponse";
import type { GitDiffMode } from "#bindings/GitDiffMode";
import type { GitDiffResult } from "#bindings/GitDiffResult";
import type { GitStatusEntry } from "#bindings/GitStatusEntry";

type GitStatusItem = {
    entry: GitStatusEntry;
    state: GitChangeKind;
};

/** Derives status sections and one de-duplicated diff order from the rendered rows. */
function groupGitStatusEntries(entries: GitStatusEntry[]) {
    const ordinaryEntries = entries.filter(
        (entry) => entry.conflict_state === null,
    );

    const conflicts = entries.flatMap<GitStatusItem>((entry) =>
        entry.conflict_state !== null ? [{ entry, state: "conflicted" }] : [],
    );

    const untracked = ordinaryEntries.flatMap<GitStatusItem>((entry) =>
        entry.index_state === "unmodified" && entry.worktree_state === "added"
            ? [{ entry, state: "untracked" }]
            : [],
    );

    const staged = ordinaryEntries.flatMap<GitStatusItem>((entry) =>
        entry.index_state !== "unmodified"
            ? [{ entry, state: entry.index_state }]
            : [],
    );

    const unstaged = ordinaryEntries.flatMap<GitStatusItem>((entry) =>
        entry.worktree_state !== "unmodified" &&
        !untracked.some((item) => item.entry.path === entry.path)
            ? [{ entry, state: entry.worktree_state }]
            : [],
    );

    const seenPaths = new Set<string>();

    const diffEntries = [
        ...conflicts,
        ...staged,
        ...unstaged,
        ...untracked,
    ].flatMap((item) => {
        const entry = item.entry;

        if (seenPaths.has(entry.path)) {
            return [];
        }

        seenPaths.add(entry.path);

        return [entry];
    });

    return { conflicts, staged, unstaged, untracked, diffEntries };
}

/** Splits a repository-relative path into its name and the directory it is in. */
function splitRelativePath(relativePath: string) {
    const slash = relativePath.lastIndexOf("/");

    return slash === -1
        ? { name: relativePath, directory: "" }
        : {
              name: relativePath.slice(slash + 1),
              directory: relativePath.slice(0, slash),
          };
}

/**
 * Lists one status group. Each row shows its state as a `git status --short` letter, the
 * file name as a link to the file's own Git view and its directory dimmed beside it, so a
 * long list can be scanned by name.
 */
function GitStatusSection(props: {
    agent: Agent;
    title: string;
    entries: GitStatusItem[];
    diffAnchorByPath: Map<string, string> | null;
}) {
    if (props.entries.length === 0) {
        return null;
    }

    const headingId = `git-${props.title.toLowerCase().replace(" ", "-")}`;

    return (
        <section aria-labelledby={headingId}>
            <h2
                id={headingId}
                className="mb-2 flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-slate-400"
            >
                {props.title}
                <span className="rounded-full bg-slate-800 px-2 py-0.5 text-[11px] font-medium normal-case tracking-normal text-slate-300">
                    {props.entries.length}
                </span>
            </h2>
            <ul className="divide-y divide-slate-800 overflow-hidden rounded-lg border border-slate-800">
                {props.entries.map((item) => {
                    const parts = splitRelativePath(
                        item.entry.repository_relative_path,
                    );

                    const anchor = props.diffAnchorByPath?.get(item.entry.path);

                    return (
                        <li
                            key={`${props.title}:${item.entry.path}`}
                            className="flex min-w-0 items-center gap-3 px-3 py-2 transition-colors hover:bg-tint"
                        >
                            <ChangeBadge kind={item.state} />
                            <span className="min-w-0 flex-1 truncate text-sm">
                                <Link
                                    {...browserRoute(
                                        props.agent,
                                        item.entry.path,
                                    )}
                                    search={{ view: "git" }}
                                    className="font-medium text-slate-100 hover:text-blue-300 hover:underline"
                                >
                                    {parts.name}
                                </Link>
                                {parts.directory === "" ? null : (
                                    <span className="ml-2 font-mono text-xs text-slate-500">
                                        {parts.directory}
                                    </span>
                                )}
                                {item.entry.original_path !== null ? (
                                    <span className="ml-2 text-xs text-slate-500">
                                        from {item.entry.original_path}
                                    </span>
                                ) : null}
                            </span>
                            {anchor === undefined ? null : (
                                <a
                                    href={`#${anchor}`}
                                    className="shrink-0 rounded px-1.5 py-0.5 text-xs font-medium text-blue-300 hover:bg-tint hover:underline"
                                >
                                    Diff
                                </a>
                            )}
                        </li>
                    );
                })}
            </ul>
        </section>
    );
}

/** Names a status group's count with a colored dot, for the summary above the lists. */
function StatusCount(props: {
    kind: GitChangeKind;
    count: number;
    label: string;
}) {
    if (props.count === 0) {
        return null;
    }

    return (
        <span className="inline-flex items-center gap-1.5 text-xs text-slate-400">
            <ChangeBadge kind={props.kind} />
            {props.count} {props.label}
        </span>
    );
}

/** Says that a path has nothing to commit, in place of empty lists. */
function CleanState(props: { message: string }) {
    return (
        <div
            role="status"
            className="flex flex-col items-center gap-2 px-4 py-12 text-center"
        >
            <CheckCircle2
                className="h-9 w-9 text-emerald-400"
                aria-hidden="true"
            />
            <p className="text-sm font-semibold text-slate-100">
                Working tree is clean
            </p>
            <p className="text-sm text-slate-400">{props.message}</p>
        </div>
    );
}

/** Keeps viewport activation scoped to one directory while preserving click fallback support. */
function useGitDiffActivation(props: {
    directoryKey: string;
    changedFileCount: number;
}) {
    const [activatedDirectory, setActivatedDirectory] = React.useState<
        string | null
    >(null);

    const loadButtonRef = React.useRef<HTMLDivElement>(null);
    const active = activatedDirectory === props.directoryKey;

    const shouldAutoLoad =
        props.changedFileCount > 0 && props.changedFileCount < 50;

    React.useEffect(() => {
        const loadButton = loadButtonRef.current;

        if (
            active ||
            !shouldAutoLoad ||
            loadButton === null ||
            !("IntersectionObserver" in window)
        ) {
            return;
        }

        const observer = new window.IntersectionObserver(
            (entries) => {
                if (!entries.some((entry) => entry.isIntersecting)) {
                    return;
                }

                setActivatedDirectory(props.directoryKey);
                observer.disconnect();
            },
            { root: null, rootMargin: "0px", threshold: 0 },
        );

        observer.observe(loadButton);

        return () => observer.disconnect();
    }, [active, props.directoryKey, shouldAutoLoad]);

    return {
        active,
        loadButtonRef,
        activate: () => setActivatedDirectory(props.directoryKey),
    };
}

/**
 * The patches of every changed file below a directory, in the order the status lists
 * them, each with an anchor the status rows link to.
 */
function GitDirectoryDiffs(props: {
    agent: Agent;
    diffEntries: GitStatusEntry[];
}) {
    const diffEntries = props.diffEntries;

    const diffQuery = useQuery(
        gitDiffQueryOptions(
            props.agent,
            diffEntries.map((entry) => entry.path),
            "full",
        ),
    );

    return (
        <div className="grid grid-cols-[minmax(0,1fr)] gap-4 border-t border-slate-800 px-4 py-4 sm:px-5">
            {diffQuery.isPending ? (
                <p role="status" className="text-sm text-slate-400">
                    Loading all Git diffs...
                </p>
            ) : diffQuery.isError ? (
                <p role="alert" className="text-sm text-red-300">
                    {getErrorMessage(
                        diffQuery.error,
                        "Failed to load Git diffs",
                    )}
                </p>
            ) : (
                diffQuery.data.diffs.map((diff, index) => {
                    const pathLabel =
                        diffEntries[index]?.repository_relative_path ??
                        diff.path;

                    const isTextDiff = diff.result.type === "text";

                    return (
                        <section
                            key={`${diff.path}:${index}`}
                            id={`git-diff-${index}`}
                            aria-label={`Git diff for ${diff.path}`}
                            className="scroll-mt-4 overflow-hidden rounded-lg border border-slate-800 bg-slate-950/30"
                        >
                            {isTextDiff ? null : (
                                <h2 className="break-all border-b border-slate-800 px-3 py-2 font-mono text-sm font-semibold">
                                    <Link
                                        {...browserRoute(
                                            props.agent,
                                            diff.path,
                                        )}
                                        className="text-blue-300 hover:underline"
                                    >
                                        {pathLabel}
                                    </Link>
                                </h2>
                            )}
                            <div className="file-diff-host git-file-diff w-full min-w-0 overflow-x-hidden [&>p]:p-3">
                                <GitDiffResultView
                                    result={diff.result}
                                    editorFile={{
                                        deviceName: props.agent.name,
                                        path: diff.path,
                                    }}
                                />
                            </div>
                        </section>
                    );
                })
            )}
        </div>
    );
}

/**
 * The worktree changes below a directory: a summary of what is pending, the files grouped
 * the way a Git workflow sees them, and their diffs on demand.
 */
export function GitDirectoryView(props: { agent: Agent; path: string }) {
    const directoryKey = `${props.agent.id}:${props.path}`;

    const statusQuery = useQuery(
        gitStatusQueryOptions(props.agent, props.path),
    );

    const groups = groupGitStatusEntries(statusQuery.data?.entries ?? []);

    const diffActivation = useGitDiffActivation({
        directoryKey,
        changedFileCount: groups.diffEntries.length,
    });

    if (statusQuery.isPending) {
        return (
            <p
                role="status"
                className="px-4 py-10 text-center text-sm text-slate-400"
            >
                Loading Git status...
            </p>
        );
    }

    if (statusQuery.isError) {
        return (
            <p
                role="alert"
                className="px-4 py-10 text-center text-sm text-red-300"
            >
                {getErrorMessage(
                    statusQuery.error,
                    "Failed to load Git status",
                )}
            </p>
        );
    }

    const status = statusQuery.data;
    const { conflicts, staged, unstaged, untracked, diffEntries } = groups;

    // Omitted or truncated entries are changes too, so their warnings below must stay visible.
    if (
        status.entries.length === 0 &&
        status.omitted_non_utf8_entries === 0 &&
        !status.truncated
    ) {
        return (
            <CleanState
                message={
                    status.path === status.repository_root
                        ? "Nothing changed compared with HEAD."
                        : "Nothing changed below this directory compared with HEAD."
                }
            />
        );
    }

    const diffAnchorByPath = diffActivation.active
        ? new Map(
              diffEntries.map((entry, index) => [
                  entry.path,
                  `git-diff-${index}`,
              ]),
          )
        : null;

    return (
        <>
            <GitPanelBar>
                <span className="font-medium text-slate-100">
                    {diffEntries.length === 1
                        ? "1 changed file"
                        : `${diffEntries.length} changed files`}
                </span>
                <StatusCount
                    kind="conflicted"
                    count={conflicts.length}
                    label="conflicted"
                />
                <StatusCount
                    kind="added"
                    count={staged.length}
                    label="staged"
                />
                <StatusCount
                    kind="modified"
                    count={unstaged.length}
                    label="unstaged"
                />
                <StatusCount
                    kind="untracked"
                    count={untracked.length}
                    label="untracked"
                />
            </GitPanelBar>
            <div className="grid grid-cols-[minmax(0,1fr)] gap-5 px-4 py-4 sm:px-5">
                {status.truncated ? (
                    <p
                        role="status"
                        className="flex items-center gap-2 rounded-lg border border-amber-400/40 bg-amber-400/10 px-3 py-2 text-sm text-amber-300"
                    >
                        <AlertTriangle className="h-4 w-4" aria-hidden="true" />
                        Status is truncated. Narrow the directory to inspect
                        more entries.
                    </p>
                ) : null}
                {status.omitted_non_utf8_entries > 0 ? (
                    <p
                        role="status"
                        className="rounded-lg border border-amber-400/40 bg-amber-400/10 px-3 py-2 text-sm text-amber-300"
                    >
                        {status.omitted_non_utf8_entries} non-UTF-8 path entries
                        were omitted.
                    </p>
                ) : null}
                <GitStatusSection
                    agent={props.agent}
                    title="Conflicts"
                    entries={conflicts}
                    diffAnchorByPath={diffAnchorByPath}
                />
                <GitStatusSection
                    agent={props.agent}
                    title="Staged changes"
                    entries={staged}
                    diffAnchorByPath={diffAnchorByPath}
                />
                <GitStatusSection
                    agent={props.agent}
                    title="Unstaged changes"
                    entries={unstaged}
                    diffAnchorByPath={diffAnchorByPath}
                />
                <GitStatusSection
                    agent={props.agent}
                    title="Untracked files"
                    entries={untracked}
                    diffAnchorByPath={diffAnchorByPath}
                />
            </div>
            {diffActivation.active ? null : (
                <div
                    ref={diffActivation.loadButtonRef}
                    className="flex flex-wrap items-center justify-between gap-3 border-t border-slate-800 px-4 py-3 sm:px-5"
                >
                    <p className="text-sm text-slate-400">
                        Diffs load when you scroll here or ask for them.
                    </p>
                    <Button
                        type="button"
                        size="sm"
                        variant="secondary"
                        onClick={diffActivation.activate}
                    >
                        Load all diffs
                    </Button>
                </div>
            )}
            {diffActivation.active && diffEntries.length > 0 ? (
                <GitDirectoryDiffs
                    agent={props.agent}
                    diffEntries={diffEntries}
                />
            ) : null}
        </>
    );
}

/** Explains all non-text outcomes rather than presenting them as empty patches. */
export function GitDiffResultView(props: {
    result: GitDiffResult;
    /** Omitted when the file has no editor to open, such as a file the diff deleted. */
    editorFile?: { deviceName: string; path: string };
}) {
    if (props.result.type === "text") {
        return (
            <UnifiedDiff
                unifiedDiff={props.result.unified_diff}
                emptyMessage="No changes in this comparison."
                editorFile={props.editorFile}
            />
        );
    }

    const messages = {
        no_changes: "No changes in this comparison.",
        untracked:
            "This file is untracked. Git has no committed version to compare.",
        ignored: "This file is ignored by Git. No diff is generated.",
        binary: "This is a binary file, so a text diff is not available.",
        too_large:
            "This file or its generated patch is too large to display safely.",
        unsupported_entry: "This Git entry cannot be displayed as a text diff.",
    } satisfies Record<Exclude<GitDiffResult["type"], "text">, string>;

    return (
        <p role="status" className="text-sm text-slate-300">
            {messages[props.result.type]}
        </p>
    );
}

/** Names how a single file relates to the repository, for the file diff's summary bar. */
function trackingLabel(context: GitContextResponse): string | null {
    if (context.status !== "inside_worktree") {
        return null;
    }

    switch (context.tracking_state) {
        case "tracked":
            return "Tracked";
        case "untracked":
            return "Untracked";
        case "ignored":
            return "Ignored";
        case "deleted":
            return "Deleted from the working tree";
        case null:
            return null;
    }
}

/**
 * One file's worktree diff against HEAD. The comparison switch sits in the panel bar,
 * where it is visible, instead of behind an overflow menu.
 */
export function GitFileView(props: {
    agent: Agent;
    path: string;
    context: GitContextResponse;
    /** Comes from the URL so the comparison survives a reload and can be shared. */
    mode: GitDiffMode;
    onModeChange: (mode: GitDiffMode) => void;
}) {
    const diffQuery = useQuery(
        gitDiffQueryOptions(props.agent, [props.path], props.mode),
    );

    const tracking = trackingLabel(props.context);

    return (
        <>
            <GitPanelBar
                actions={
                    <SegmentedControl
                        label="Compared changes"
                        className="w-64"
                        value={props.mode}
                        onChange={props.onModeChange}
                        options={[
                            {
                                value: "full",
                                label: "All changes",
                                icon: FileDiff,
                                tooltip:
                                    "Working tree and staged changes compared with HEAD",
                            },
                            {
                                value: "staged",
                                label: "Staged only",
                                icon: ListChecks,
                                tooltip:
                                    "Only the changes staged in the index, compared with HEAD",
                            },
                        ]}
                    />
                }
            >
                <span className="min-w-0 truncate font-mono font-medium text-slate-100">
                    {getBaseName(props.path)}
                </span>
                {tracking === null ? null : (
                    <span className="rounded-full border border-slate-700 px-2 py-0.5 text-xs text-slate-400">
                        {tracking}
                    </span>
                )}
            </GitPanelBar>
            <section
                aria-label={`${props.mode === "full" ? "Full" : "Staged"} Git diff`}
                className="file-diff-host git-file-diff w-full min-w-0 overflow-x-hidden bg-slate-950/30 [&>p]:p-4 sm:[&>p]:p-5"
            >
                {diffQuery.isPending ? (
                    <p role="status" className="p-4 text-sm text-slate-400">
                        Loading {props.mode} Git diff...
                    </p>
                ) : diffQuery.isError ? (
                    <p role="alert" className="p-4 text-sm text-red-300">
                        {getErrorMessage(
                            diffQuery.error,
                            "Failed to load Git diff",
                        )}
                    </p>
                ) : (
                    <GitDiffResultView
                        result={
                            diffQuery.data.diffs[0]?.result ?? {
                                type: "no_changes",
                            }
                        }
                        editorFile={{
                            deviceName: props.agent.name,
                            path: props.path,
                        }}
                    />
                )}
            </section>
        </>
    );
}
