import React from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useAtomValue } from "jotai";
import {
    AlertTriangle,
    CheckCircle2,
    FileDiff,
    ListChecks,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { SegmentedControl } from "#ui/components/segmented-control";
import { UnifiedDiff } from "#ui/components/browser/unified-diff";
import { DiffLayoutControl } from "#ui/components/browser/diff-layout-control";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";
import { useUserState } from "#ui/user-state";
import {
    GitDiffCollapseControls,
    GitDiffList,
    useCollapsedGitDiffs,
    type GitDiffListFile,
} from "#ui/components/browser/git-diff-list";
import {
    ChangeBadge,
    GitPanelBar,
    type GitChangeKind,
} from "#ui/components/browser/git-parts";
import {
    gitDiffQueryOptions,
    gitStatusQueryOptions,
    prefetchGitDiffs,
} from "#ui/queries";
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

/** Derives the status counts and one de-duplicated diff order, conflicts first. */
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

    // A file both staged and modified again is listed once, as its most pressing state.
    const diffItems = [
        ...conflicts,
        ...staged,
        ...unstaged,
        ...untracked,
    ].flatMap((item) => {
        if (seenPaths.has(item.entry.path)) {
            return [];
        }

        seenPaths.add(item.entry.path);

        return [item];
    });

    return { conflicts, staged, unstaged, untracked, diffItems };
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

/**
 * One file's patch in the worktree diff list. It mounts only when the file is expanded, so
 * a diff is fetched the first time its file is opened, and then comes from the cache.
 */
function WorktreeDiffBody(props: { agent: Agent; path: string }) {
    const diffQuery = useQuery(
        gitDiffQueryOptions(props.agent, [props.path], "full"),
    );

    if (diffQuery.isPending) {
        return (
            <p role="status" className="text-sm text-slate-400">
                Loading Git diff...
            </p>
        );
    }

    if (diffQuery.isError) {
        return (
            <p role="alert" className="text-sm text-red-300">
                {getErrorMessage(diffQuery.error, "Failed to load Git diff")}
            </p>
        );
    }

    return (
        <GitDiffResultView
            result={diffQuery.data.diffs[0]?.result ?? { type: "no_changes" }}
            editorFile={{ deviceName: props.agent.name, path: props.path }}
        />
    );
}

/**
 * The worktree changes below a directory: a summary of what is pending and every changed
 * file under a header naming it, collapsed until its diff is wanted.
 */
export function GitDirectoryView(props: { agent: Agent; path: string }) {
    const statusQuery = useQuery(
        gitStatusQueryOptions(props.agent, props.path),
    );

    const queryClient = useQueryClient();

    // Diffs load per file on expansion, so a large change set opens without fetching any.
    const collapsed = useCollapsedGitDiffs({
        scope: `${props.agent.id}:${props.path}`,
        initiallyCollapsed: true,
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

    const { conflicts, staged, unstaged, untracked, diffItems } =
        groupGitStatusEntries(status.entries);

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

    const files = diffItems.map((item): GitDiffListFile => {
        const entry = item.entry;

        return {
            path: entry.path,
            relativePath: entry.repository_relative_path,
            originalPath: entry.original_path,
            change: item.state,
            // A file deleted from the worktree still exists at HEAD, so that is where it opens.
            fileSearch: item.state === "deleted" ? { rev: "HEAD" } : {},
            regionLabel: `Git diff for ${entry.path}`,
            body: <WorktreeDiffBody agent={props.agent} path={entry.path} />,
        };
    });

    return (
        <>
            <GitPanelBar
                actions={
                    <>
                        {files.length === 0 ? null : <DiffLayoutControl />}
                        <GitDiffCollapseControls
                            collapsed={collapsed}
                            fileCount={files.length}
                            onExpandAll={() =>
                                prefetchGitDiffs({
                                    queryClient,
                                    agent: props.agent,
                                    files: files.map((file) => file.path),
                                    mode: "full",
                                })
                            }
                        />
                    </>
                }
            >
                <span className="font-medium text-slate-100">
                    {diffItems.length === 1
                        ? "1 changed file"
                        : `${diffItems.length} changed files`}
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
            <div className="grid grid-cols-[minmax(0,1fr)] gap-4 px-4 py-4 sm:px-5">
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
                <GitDiffList
                    agent={props.agent}
                    files={files}
                    collapsed={collapsed}
                />
            </div>
        </>
    );
}

/** Explains all non-text outcomes rather than presenting them as empty patches. */
export function GitDiffResultView(props: {
    result: GitDiffResult;
    /** Omitted when the file has no editor to open, such as a file the diff deleted. */
    editorFile?: { deviceName: string; path: string };
}) {
    // Every Git diff follows the one layout preference its view's bar switches.
    const mergeModifiedLines = useAtomValue(inlineDiffChangesAtom);
    const [userState] = useUserState();

    if (props.result.type === "text") {
        return (
            <UnifiedDiff
                unifiedDiff={props.result.unified_diff}
                emptyMessage="No changes in this comparison."
                editorFile={props.editorFile}
                mergeModifiedLines={mergeModifiedLines}
                wrapLines={userState.wrapDiffLines}
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
                    <>
                        <DiffLayoutControl />
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
                    </>
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
                className="w-full min-w-0 overflow-x-hidden bg-slate-950/30 [&>p]:p-4 sm:[&>p]:p-5"
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
