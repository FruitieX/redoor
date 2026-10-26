import React from "react";
import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
    AlertTriangle,
    GitCommitHorizontal,
    History,
    Undo2,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { Tooltip } from "#ui/components/tooltip";
import { GitDiffResultView } from "#ui/components/browser/git";
import {
    AuthorAvatar,
    ChangeBadge,
    CommitAge,
    GitPanelBar,
    changeLabel,
    displayRevision,
    shortCommitId,
} from "#ui/components/browser/git-parts";
import { formatModifiedAge } from "#ui/components/browser/utils";
import { gitCommitDiffQueryOptions } from "#ui/queries";
import { getErrorMessage } from "#ui/utils/error";
import type { GitChangeState } from "#bindings/GitChangeState";
import type { GitCommitDiffResponse } from "#bindings/GitCommitDiffResponse";
import type { GitCommitSummary } from "#bindings/GitCommitSummary";
import type { GitRevisionEntry } from "#bindings/GitRevisionEntry";
import type { GitTreeEntry } from "#bindings/GitTreeEntry";
import type { LsEntry } from "#bindings/LsEntry";

/**
 * Marks the page as a read-only view of history, directly below the breadcrumbs, so it is
 * impossible to mistake committed content for the working files. It also offers the way
 * back to the working tree, which drops the revision from the URL.
 */
export function RevisionBanner(props: {
    agent: Agent;
    path: string;
    rev: string;
    /** Absent while the revision could not be resolved. */
    commit: GitCommitSummary | null;
}) {
    const now = Date.now();
    const commit = props.commit;
    const showsCommitId = commit !== null && !commit.id.startsWith(props.rev);

    return (
        <section
            aria-label="Git revision"
            className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-2 rounded-md border border-amber-700/60 bg-amber-950/30 px-3 py-2 text-sm"
        >
            <span className="inline-flex items-center gap-1.5 rounded-full border border-amber-600/70 bg-amber-900/40 px-2 py-0.5 text-xs font-semibold text-amber-200">
                <History className="h-3.5 w-3.5" aria-hidden="true" />
                History
            </span>
            <span className="min-w-0 text-amber-100">
                Viewing{" "}
                <code
                    aria-label="Revision"
                    title={props.rev}
                    className="rounded bg-amber-900/40 px-1 font-mono text-amber-50"
                >
                    {displayRevision(props.rev)}
                </code>
                {showsCommitId ? (
                    <span className="ml-1 font-mono text-xs text-amber-200/80">
                        ({shortCommitId(commit.id)})
                    </span>
                ) : null}
            </span>
            {commit === null ? null : (
                <span className="min-w-0 flex-1 truncate text-slate-300">
                    <span title={commit.summary}>{commit.summary}</span>
                    <span className="text-slate-500">
                        {" "}
                        · {commit.author_name} ·{" "}
                        <time
                            dateTime={new Date(
                                commit.authored_at * 1000,
                            ).toISOString()}
                            title={new Date(
                                commit.authored_at * 1000,
                            ).toLocaleString()}
                        >
                            {formatModifiedAge(commit.authored_at, now)}
                        </time>
                    </span>
                </span>
            )}
            <span className="text-xs text-amber-200/80">Read-only</span>
            <div className="ml-auto flex shrink-0 items-center gap-1">
                {commit === null ? null : (
                    <Tooltip content="Show the changes this commit made">
                        <Link
                            {...browserRoute(props.agent, props.path)}
                            search={{ view: "git", rev: props.rev }}
                            className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-amber-100 hover:bg-amber-900/50"
                        >
                            <GitCommitHorizontal
                                className="h-3.5 w-3.5"
                                aria-hidden="true"
                            />
                            Commit changes
                        </Link>
                    </Tooltip>
                )}
                <Tooltip content="Leave history and show the current working files">
                    <Link
                        {...browserRoute(props.agent, props.path)}
                        search={{ rev: undefined }}
                        className="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-amber-100 hover:bg-amber-900/50"
                    >
                        <Undo2 className="h-3.5 w-3.5" aria-hidden="true" />
                        Working tree
                    </Link>
                </Tooltip>
            </div>
        </section>
    );
}

/**
 * Converts a committed tree listing into file-list rows, so a revision is listed by the same
 * component as a working directory. Git records no owner, group or modification time, so
 * those stay empty and the read-only list does not show them. Submodules are directories in
 * a checkout, so they list among the directories.
 */
export function revisionListingEntries(entries: GitTreeEntry[]): LsEntry[] {
    return entries.map((entry) => ({
        name: entry.name,
        type:
            entry.kind === "directory" || entry.kind === "submodule"
                ? "directory"
                : "file",
        size: entry.size ?? 0,
        owner: null,
        group: null,
        uid: 0,
        gid: 0,
        modified_at: 0,
    }));
}

/**
 * The Git revision a listing shows. Its entries can be selected and downloaded at the
 * commit, except names Git records as symlinks or submodules: Git stores a symlink as its
 * target and a submodule as a commit id, so neither has content a copy or download could
 * write.
 */
export type RevisionListing = {
    commitId: string;
    nonTransferableNames: ReadonlySet<string>;
};

/** Describes a committed directory listing for the shared file list. */
export function revisionListing(
    commitId: string,
    entry: Extract<GitRevisionEntry, { type: "directory" }>,
): RevisionListing {
    return {
        commitId,
        nonTransferableNames: new Set(
            entry.entries.flatMap((child) =>
                child.kind === "symlink" || child.kind === "submodule"
                    ? [child.name]
                    : [],
            ),
        ),
    };
}

/**
 * Explains a committed listing that is not complete. Without it a directory cut at the
 * listing limit, or one with names that cannot be linked, would look like it has no more
 * entries than are shown.
 */
export function RevisionListingNotice(props: {
    entry: Extract<GitRevisionEntry, { type: "directory" }>;
}) {
    const omitted = props.entry.omitted_non_utf8_entries;

    if (!props.entry.truncated && omitted === 0) {
        return null;
    }

    return (
        <div
            role="status"
            className="m-2 grid gap-1 rounded-lg border border-amber-700/60 bg-amber-950/30 p-3 text-sm text-amber-200 lg:m-4"
        >
            {props.entry.truncated ? (
                <p className="flex items-center gap-2">
                    <AlertTriangle className="h-4 w-4" aria-hidden="true" />
                    This directory has more entries than can be listed. Only the
                    first {props.entry.entries.length} are shown.
                </p>
            ) : null}
            {omitted === 0 ? null : (
                <p className="flex items-center gap-2">
                    <AlertTriangle className="h-4 w-4" aria-hidden="true" />
                    {omitted === 1
                        ? "1 entry is hidden because its name is not valid UTF-8."
                        : `${omitted} entries are hidden because their names are not valid UTF-8.`}
                </p>
            )}
        </div>
    );
}

/** Counts a commit's changed files per kind, in a stable order for the summary bar. */
function countChanges(diff: GitCommitDiffResponse) {
    const order: GitChangeState[] = [
        "added",
        "modified",
        "deleted",
        "renamed",
        "copied",
        "type_changed",
    ];

    return order.flatMap((kind) => {
        const count = diff.files.filter((file) => file.change === kind).length;

        return count === 0 ? [] : [{ kind, count }];
    });
}

/**
 * Shows what the revision's commit changed below the browsed path, compared with its first
 * parent: who made the commit and when, a summary and index of the changed files, then each
 * file's diff. File names link to the file at the commit, or at the parent for deleted
 * files, which no longer exist at the commit.
 */
export function GitCommitView(props: {
    agent: Agent;
    path: string;
    /** The revision as browsed, which links keep so they follow a branch rather than pin it. */
    rev: string;
    /** The commit `rev` resolved to, which the diff is fetched and cached by. */
    commitId: string;
}) {
    const diffQuery = useQuery(
        gitCommitDiffQueryOptions(props.agent, props.path, props.commitId),
    );

    if (diffQuery.isPending) {
        return (
            <p
                role="status"
                className="px-4 py-10 text-center text-sm text-slate-400"
            >
                Loading commit changes...
            </p>
        );
    }

    if (diffQuery.isError) {
        return (
            <p
                role="alert"
                className="px-4 py-10 text-center text-sm text-red-300"
            >
                {getErrorMessage(
                    diffQuery.error,
                    "Failed to load commit changes",
                )}
            </p>
        );
    }

    const diff = diffQuery.data;
    const commit = diff.commit;
    const parentId = diff.parent_id;
    const now = Date.now();

    /** Deleted files no longer exist at the commit, so they open at the parent. */
    const fileRevision = (change: GitChangeState) =>
        change === "deleted" && parentId !== null ? parentId : props.rev;

    return (
        <>
            <header className="flex items-start gap-3 border-b border-slate-800 px-4 py-4 sm:px-5">
                <AuthorAvatar name={commit.author_name} size="md" />
                <div className="min-w-0 flex-1">
                    <h2 className="break-words text-base font-semibold text-slate-50">
                        {commit.summary || "(no message)"}
                    </h2>
                    <p className="mt-1 text-sm text-slate-400">
                        <span
                            className="font-medium text-slate-200"
                            title={commit.author_email}
                        >
                            {commit.author_name}
                        </span>{" "}
                        committed{" "}
                        <CommitAge authoredAt={commit.authored_at} now={now} />
                    </p>
                    <p className="mt-2 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-slate-500">
                        {parentId === null ? (
                            "Root commit: every file was added."
                        ) : (
                            <>
                                {commit.parent_ids.length > 1
                                    ? "Merge, compared with first parent"
                                    : "Parent"}
                                <Link
                                    {...browserRoute(props.agent, props.path)}
                                    search={{ view: "git", rev: parentId }}
                                    className="rounded-md border border-slate-700 bg-slate-900/60 px-1.5 py-0.5 font-mono text-blue-300 hover:border-blue-400/50 hover:underline"
                                >
                                    {shortCommitId(parentId)}
                                </Link>
                            </>
                        )}
                    </p>
                </div>
                <code
                    title={commit.id}
                    className="shrink-0 rounded-md border border-slate-700 bg-slate-900/60 px-2 py-1 font-mono text-xs text-slate-300"
                >
                    {shortCommitId(commit.id)}
                </code>
            </header>
            <GitPanelBar>
                <span className="font-medium text-slate-100">
                    {diff.files.length === 1
                        ? "1 changed file"
                        : `${diff.files.length}${diff.truncated ? "+" : ""} changed files`}
                </span>
                {countChanges(diff).map((entry) => (
                    <span
                        key={entry.kind}
                        className="inline-flex items-center gap-1.5 text-xs text-slate-400"
                    >
                        <ChangeBadge kind={entry.kind} />
                        {entry.count} {changeLabel(entry.kind).toLowerCase()}
                    </span>
                ))}
                {diff.repository_relative_path === "" ? null : (
                    <span className="text-xs text-slate-500">
                        Only changes to{" "}
                        <code className="font-mono text-slate-300">
                            {diff.repository_relative_path}
                        </code>
                    </span>
                )}
            </GitPanelBar>
            <div className="grid grid-cols-[minmax(0,1fr)] gap-4 px-4 py-4 sm:px-5">
                {diff.files.length === 0 ? (
                    <p role="status" className="text-sm text-slate-300">
                        This commit changed nothing below this path.
                    </p>
                ) : null}
                {diff.truncated ? (
                    <p
                        role="status"
                        className="flex items-center gap-2 rounded-lg border border-amber-400/40 bg-amber-400/10 px-3 py-2 text-sm text-amber-300"
                    >
                        <AlertTriangle className="h-4 w-4" aria-hidden="true" />
                        The commit changed more files than can be shown. Open a
                        subdirectory to see the rest.
                    </p>
                ) : null}
                {diff.files.length > 1 ? (
                    <ul
                        aria-label="Changed files"
                        className="divide-y divide-slate-800 overflow-hidden rounded-lg border border-slate-800"
                    >
                        {diff.files.map((file, index) => (
                            <li
                                key={file.path}
                                className="flex min-w-0 items-center gap-3 px-3 py-1.5 transition-colors hover:bg-tint"
                            >
                                <ChangeBadge kind={file.change} />
                                <Link
                                    {...browserRoute(props.agent, file.path)}
                                    search={{ rev: fileRevision(file.change) }}
                                    className="min-w-0 flex-1 truncate font-mono text-sm text-slate-200 hover:text-blue-300 hover:underline"
                                >
                                    {file.repository_relative_path}
                                </Link>
                                <a
                                    href={`#commit-diff-${index}`}
                                    className="shrink-0 rounded px-1.5 py-0.5 text-xs font-medium text-blue-300 hover:bg-tint hover:underline"
                                >
                                    Diff
                                </a>
                            </li>
                        ))}
                    </ul>
                ) : null}
                {diff.files.map((file, index) => (
                    <section
                        key={file.path}
                        id={`commit-diff-${index}`}
                        aria-label={`Commit diff for ${file.repository_relative_path}`}
                        className="scroll-mt-4 overflow-hidden rounded-lg border border-slate-800 bg-slate-950/30"
                    >
                        {/* A text patch names its file in its own header, like the worktree Git view. */}
                        {file.result.type === "text" ? null : (
                            <h3 className="flex min-w-0 items-center gap-3 border-b border-slate-800 px-3 py-2 font-mono text-sm font-semibold">
                                <ChangeBadge kind={file.change} />
                                <Link
                                    {...browserRoute(props.agent, file.path)}
                                    search={{ rev: fileRevision(file.change) }}
                                    className="min-w-0 break-all text-blue-300 hover:underline"
                                >
                                    {file.repository_relative_path}
                                </Link>
                            </h3>
                        )}
                        <div className="file-diff-host git-file-diff w-full min-w-0 overflow-x-hidden [&>p]:p-3">
                            <GitDiffResultView
                                result={file.result}
                                editorFile={
                                    file.change === "deleted"
                                        ? undefined
                                        : {
                                              deviceName: props.agent.name,
                                              path: file.path,
                                          }
                                }
                            />
                        </div>
                    </section>
                ))}
            </div>
        </>
    );
}
