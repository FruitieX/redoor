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
import { BrowserViewCard } from "#ui/components/browser-view-card";
import { ViewCardHeader } from "#ui/components/view-card-header";
import { Tooltip } from "#ui/components/tooltip";
import { GitDiffResultView, RepositoryLink } from "#ui/components/browser/git";
import { formatModifiedAge } from "#ui/components/browser/utils";
import { gitCommitDiffQueryOptions } from "#ui/queries";
import { getErrorMessage } from "#ui/utils/error";
import type { GitChangeState } from "#bindings/GitChangeState";
import type { GitCommitSummary } from "#bindings/GitCommitSummary";
import type { GitTreeEntry } from "#bindings/GitTreeEntry";
import type { LsEntry } from "#bindings/LsEntry";

/** Shortens a commit id the way Git tools display it. */
function shortCommitId(id: string): string {
    return id.slice(0, 12);
}

/** Full commit ids are unreadable in a banner, so they are shortened; names stay as typed. */
function displayRevision(rev: string): string {
    return /^[0-9a-f]{40,64}$/i.test(rev) ? shortCommitId(rev) : rev;
}

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

/** Converts the shared change vocabulary into short labels for repeated rows. */
function formatChange(change: GitChangeState): string {
    return change.replace("_", " ");
}

/**
 * Shows what the revision's commit changed below the browsed path, compared with its first
 * parent. File names link to the file at the commit, or at the parent for deleted files,
 * which no longer exist at the commit.
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
            <p role="status" className="p-4 text-sm text-slate-400">
                Loading commit changes...
            </p>
        );
    }

    if (diffQuery.isError) {
        return (
            <p role="alert" className="p-4 text-sm text-red-300">
                {getErrorMessage(
                    diffQuery.error,
                    "Failed to load commit changes",
                )}
            </p>
        );
    }

    const diff = diffQuery.data;
    const parentId = diff.parent_id;

    return (
        <BrowserViewCard>
            <ViewCardHeader
                icon={
                    <GitCommitHorizontal
                        className="h-5 w-5"
                        aria-hidden="true"
                    />
                }
                eyebrow={`Commit ${shortCommitId(diff.commit.id)}`}
                title={diff.commit.summary || "(no message)"}
            >
                <p className="mt-2 text-sm text-slate-400">
                    {diff.commit.author_name} ·{" "}
                    {new Date(diff.commit.authored_at * 1000).toLocaleString()}
                </p>
                <p className="mt-1 text-sm text-slate-400">
                    {parentId === null ? (
                        "Root commit: every file was added."
                    ) : (
                        <>
                            Compared with parent{" "}
                            <Link
                                {...browserRoute(props.agent, props.path)}
                                search={{ view: "git", rev: parentId }}
                                className="font-mono text-blue-300 hover:underline"
                            >
                                {shortCommitId(parentId)}
                            </Link>
                        </>
                    )}
                </p>
                {diff.repository_relative_path === "" ? null : (
                    <>
                        <p className="mt-1 text-sm text-slate-400">
                            Only changes to{" "}
                            <code className="font-mono text-slate-200">
                                {diff.repository_relative_path}
                            </code>{" "}
                            are shown.
                        </p>
                        {/* Like the worktree Git view, the whole commit is one link away. */}
                        <RepositoryLink
                            agent={props.agent}
                            repositoryRoot={diff.repository_root}
                        />
                    </>
                )}
            </ViewCardHeader>
            <div className="grid gap-4 p-4 sm:p-5">
                {diff.files.length === 0 ? (
                    <p role="status" className="text-sm text-slate-300">
                        This commit changed nothing below this path.
                    </p>
                ) : null}
                {diff.truncated ? (
                    <p
                        role="status"
                        className="flex items-center gap-2 text-sm text-amber-300"
                    >
                        <AlertTriangle className="h-4 w-4" aria-hidden="true" />
                        The commit changed more files than can be shown. Open a
                        subdirectory to see the rest.
                    </p>
                ) : null}
                {diff.files.map((file) => {
                    const isDeleted = file.change === "deleted";

                    return (
                        <section
                            key={file.path}
                            aria-label={`Commit diff for ${file.repository_relative_path}`}
                            className="overflow-hidden rounded-md border border-slate-800 bg-slate-950/30"
                        >
                            {/* A text patch names its file in its own header, like the worktree Git view. */}
                            {file.result.type === "text" ? null : (
                                <h2 className="flex min-w-0 items-center justify-between gap-3 border-b border-slate-800 px-3 py-2 font-mono text-sm font-semibold">
                                    <Link
                                        {...browserRoute(
                                            props.agent,
                                            file.path,
                                        )}
                                        search={{
                                            rev:
                                                isDeleted && parentId !== null
                                                    ? parentId
                                                    : props.rev,
                                        }}
                                        className="min-w-0 break-all text-blue-300 hover:underline"
                                    >
                                        {file.repository_relative_path}
                                    </Link>
                                    <span className="shrink-0 rounded-full border border-slate-700 px-2 py-0.5 font-sans text-[11px] font-normal capitalize text-slate-400 sm:text-xs">
                                        {formatChange(file.change)}
                                    </span>
                                </h2>
                            )}
                            <div className="file-diff-host git-file-diff w-full min-w-0 overflow-x-hidden [&>p]:p-3">
                                <GitDiffResultView
                                    result={file.result}
                                    editorFile={
                                        isDeleted
                                            ? undefined
                                            : {
                                                  deviceName: props.agent.name,
                                                  path: file.path,
                                              }
                                    }
                                />
                            </div>
                        </section>
                    );
                })}
            </div>
        </BrowserViewCard>
    );
}
