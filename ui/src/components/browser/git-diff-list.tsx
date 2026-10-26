import React from "react";
import { Link } from "@tanstack/react-router";
import { ChevronDown, ChevronRight } from "lucide-react";
import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { Button } from "#ui/components/button";
import { IconButton } from "#ui/components/icon-button";
import {
    ChangeBadge,
    type GitChangeKind,
} from "#ui/components/browser/git-parts";

/** One changed file in a diff list, already resolved to what its header and body show. */
export type GitDiffListFile = {
    /** The absolute path, which is also the file's identity for collapsing. */
    path: string;
    /** The repository-relative path, which names the file in its header. */
    relativePath: string;
    /** The path a rename or copy came from, shown beside the name. */
    originalPath: string | null;
    change: GitChangeKind;
    /**
     * The search the header link opens the file with, such as the revision a committed file
     * exists at. The link always opens the file itself, never its diff, because the diff is
     * already on screen.
     */
    fileSearch: { rev?: string };
    /** Names the file's region, so tests and assistive technology can find one patch. */
    regionLabel: string;
    /**
     * The patch or a message about it. It is only rendered while the file is expanded, so a
     * body that fetches its own diff loads nothing for a collapsed file.
     */
    body: React.ReactNode;
};

/**
 * Remembers which diffs of a list are collapsed. The state is a default plus the files toggled
 * away from it, so collapsing or expanding all is one change that also covers files the list
 * gains later. It belongs to one scope, such as one directory's changes, and starts over when
 * the scope changes, because the same view stays mounted while browsing to another directory.
 */
export function useCollapsedGitDiffs(options: {
    scope: string;
    initiallyCollapsed: boolean;
}) {
    const fresh = {
        scope: options.scope,
        collapsedByDefault: options.initiallyCollapsed,
        toggled: new Set<string>(),
    };

    const [state, setState] = React.useState<{
        scope: string;
        collapsedByDefault: boolean;
        toggled: ReadonlySet<string>;
    }>(fresh);

    const current = state.scope === options.scope ? state : fresh;

    return {
        isCollapsed: (path: string) =>
            current.collapsedByDefault !== current.toggled.has(path),
        toggle: (path: string) =>
            setState((previous) => {
                const base =
                    previous.scope === options.scope ? previous : fresh;

                const toggled = new Set(base.toggled);

                if (!toggled.delete(path)) {
                    toggled.add(path);
                }

                return { ...base, toggled };
            }),
        collapseAll: () => setState({ ...fresh, collapsedByDefault: true }),
        expandAll: () => setState({ ...fresh, collapsedByDefault: false }),
    };
}

/** The collapse state a diff list and its controls share. */
export type CollapsedGitDiffs = ReturnType<typeof useCollapsedGitDiffs>;

/**
 * Collapses or expands every diff at once, for the panel bar above a list, so a long change
 * set can be folded down to its file names and opened again without clicking each file.
 */
export function GitDiffCollapseControls(props: {
    collapsed: CollapsedGitDiffs;
    fileCount: number;
    /**
     * Runs before every file expands, so a list that loads diffs on demand can fetch all the
     * missing ones in one request instead of one request per file.
     */
    onExpandAll?: () => void;
}) {
    if (props.fileCount === 0) {
        return null;
    }

    return (
        <>
            <Button
                type="button"
                size="sm"
                variant="secondary"
                tooltip="Hide every patch and show only the file names"
                onClick={props.collapsed.collapseAll}
            >
                Collapse all
            </Button>
            <Button
                type="button"
                size="sm"
                variant="secondary"
                tooltip="Show the patch of every file"
                onClick={() => {
                    props.onExpandAll?.();
                    props.collapsed.expandAll();
                }}
            >
                Expand all
            </Button>
        </>
    );
}

/**
 * Splits a repository-relative path into its name and its directory, so the header can lead
 * with the name and dim the directory beside it.
 */
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
 * One file's diff. Its header sticks to the top of the viewport while the patch scrolls
 * under it, so a long patch never leaves the reader guessing which file it belongs to. The
 * section clips instead of hiding overflow, because a hidden overflow would become the
 * header's scroll container and stop it from sticking.
 */
function GitDiffSection(props: {
    agent: Agent;
    file: GitDiffListFile;
    collapsed: boolean;
    onToggle: () => void;
}) {
    const file = props.file;
    const parts = splitRelativePath(file.relativePath);
    const bodyId = React.useId();

    return (
        <section
            aria-label={file.regionLabel}
            className="overflow-clip rounded-lg border border-slate-800 bg-slate-950/30"
        >
            <div
                className={`git-diff-sticky-header sticky z-10 flex min-w-0 items-center gap-3 bg-surface px-3 py-2 ${props.collapsed ? "" : "border-b border-slate-800"}`}
            >
                <IconButton
                    type="button"
                    variant="ghost"
                    size="xs"
                    label={
                        props.collapsed
                            ? `Expand diff for ${file.relativePath}`
                            : `Collapse diff for ${file.relativePath}`
                    }
                    tooltip={props.collapsed ? "Show patch" : "Hide patch"}
                    aria-expanded={!props.collapsed}
                    aria-controls={bodyId}
                    onClick={props.onToggle}
                >
                    {props.collapsed ? (
                        <ChevronRight className="h-4 w-4" aria-hidden="true" />
                    ) : (
                        <ChevronDown className="h-4 w-4" aria-hidden="true" />
                    )}
                </IconButton>
                <ChangeBadge kind={file.change} />
                <h3 className="min-w-0 flex-1 truncate text-sm">
                    <Link
                        {...browserRoute(props.agent, file.path)}
                        search={file.fileSearch}
                        title={file.relativePath}
                        className="font-medium text-slate-100 hover:text-blue-300 hover:underline"
                    >
                        {parts.name}
                    </Link>
                    {parts.directory === "" ? null : (
                        <span className="ml-2 font-mono text-xs text-slate-500">
                            {parts.directory}
                        </span>
                    )}
                    {file.originalPath === null ? null : (
                        <span className="ml-2 text-xs text-slate-500">
                            from {file.originalPath}
                        </span>
                    )}
                </h3>
            </div>
            {props.collapsed ? null : (
                <div
                    id={bodyId}
                    className="file-diff-host git-file-diff git-diff-list-body w-full min-w-0 overflow-x-hidden [&>p]:p-3"
                >
                    {file.body}
                </div>
            )}
        </section>
    );
}

/**
 * Every changed file's diff in one scrolling list. The headers replace the separate file
 * list, so each file is named exactly once, right above its own patch.
 */
export function GitDiffList(props: {
    agent: Agent;
    files: GitDiffListFile[];
    collapsed: CollapsedGitDiffs;
}) {
    return (
        <div className="grid grid-cols-[minmax(0,1fr)] gap-4">
            {props.files.map((file) => (
                <GitDiffSection
                    key={file.path}
                    agent={props.agent}
                    file={file}
                    collapsed={props.collapsed.isCollapsed(file.path)}
                    onToggle={() => props.collapsed.toggle(file.path)}
                />
            ))}
        </div>
    );
}
