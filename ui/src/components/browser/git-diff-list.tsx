import React from "react";
import { Link } from "@tanstack/react-router";
import { useAtomValue } from "jotai";
import {
    Check,
    ChevronDown,
    ChevronRight,
    ChevronsDownUp,
    ChevronsUpDown,
    Copy,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { ActionMenu, ActionMenuButton } from "#ui/components/action-menu";
import { Button } from "#ui/components/button";
import { ClearableInput } from "#ui/components/clearable-input";
import { useCopyFeedback } from "#ui/components/copyable-code-row";
import { IconButton } from "#ui/components/icon-button";
import {
    DiffLayoutSwitch,
    WrapDiffLinesToggle,
} from "#ui/components/browser/diff-layout-control";
import {
    ChangeBadge,
    type GitChangeKind,
} from "#ui/components/browser/git-parts";
import { inlineDiffChangesAtom } from "#ui/diff-preferences";
import { cn } from "#ui/utils/cn";
import type { FileNameFilter } from "#ui/utils/file-name-filter";

/** How one file of a diff list shows its patch, as chosen in the file's header. */
export type GitDiffBodyView = {
    /** Whether a similar removed and added line show as one row with inline changes. */
    mergeModifiedLines: boolean;
    /** Whether long lines wrap instead of scrolling the patch sideways. */
    wrapLines: boolean;
};

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
     * Renders the patch or a message about it in the file's own layout. It is only called
     * while the file is expanded, so a body that fetches its own diff loads nothing for a
     * collapsed file.
     */
    renderBody: (view: GitDiffBodyView) => React.ReactNode;
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
 * Remembers the layout chosen for each file of a list, because one file's edits read best as
 * inline word changes while a rewritten file reads best as separate lines. Files without a
 * choice follow the stored preference, and the choices start over with the scope, like the
 * collapse state, since the same paths in another commit are other edits.
 */
function useGitDiffLayouts(scope: string) {
    const inlineByDefault = useAtomValue(inlineDiffChangesAtom);

    const [state, setState] = React.useState<{
        scope: string;
        inlineByPath: ReadonlyMap<string, boolean>;
    }>({ scope, inlineByPath: new Map() });

    const inlineByPath =
        state.scope === scope ? state.inlineByPath : new Map<string, boolean>();

    return {
        isInline: (path: string) => inlineByPath.get(path) ?? inlineByDefault,
        setInline: (path: string, inline: boolean) =>
            setState((previous) => {
                const next = new Map(
                    previous.scope === scope ? previous.inlineByPath : [],
                );

                next.set(path, inline);

                return { scope, inlineByPath: next };
            }),
    };
}

/**
 * Narrows a diff list by file name, so a large change set can be read one area at a time or
 * without its tests (`-test`). It names how many files the filter keeps, because a hidden
 * file would otherwise look like one the change never touched.
 */
export function GitDiffFilterField(props: {
    query: string;
    onChange: (query: string) => void;
    filter: FileNameFilter;
    shownCount: number;
    totalCount: number;
}) {
    return (
        <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2">
            <ClearableInput
                label="Filter files by name"
                clearLabel="Clear file filter"
                value={props.query}
                placeholder='Filter files, -test excludes, "-test" is literal'
                onChange={props.onChange}
                className="min-w-0 flex-1 sm:max-w-md"
            />
            {props.filter.isActive ? (
                <span role="status" className="text-xs text-slate-400">
                    {props.shownCount === 0
                        ? "No files match the filter"
                        : `Showing ${props.shownCount} of ${props.totalCount} files`}
                </span>
            ) : null}
        </div>
    );
}

/**
 * Collapses or expands every diff at once, for the panel bar above a list, so a long change
 * set can be folded down to its file names and opened again without clicking each file. On
 * phones the buttons shrink to their icons so they share one bar with the file counts.
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
                aria-label="Collapse all"
                tooltip="Hide every patch and show only the file names"
                className="gap-1.5 px-2 py-1.5 sm:px-3 sm:py-2"
                onClick={props.collapsed.collapseAll}
            >
                <ChevronsDownUp className="h-4 w-4" aria-hidden="true" />
                <span className="hidden sm:inline">Collapse all</span>
            </Button>
            <Button
                type="button"
                size="sm"
                variant="secondary"
                aria-label="Expand all"
                tooltip="Show the patch of every file"
                className="gap-1.5 px-2 py-1.5 sm:px-3 sm:py-2"
                onClick={() => {
                    props.onExpandAll?.();
                    props.collapsed.expandAll();
                }}
            >
                <ChevronsUpDown className="h-4 w-4" aria-hidden="true" />
                <span className="hidden sm:inline">Expand all</span>
            </Button>
        </>
    );
}

/**
 * Splits a repository-relative path into its name and its directory, so the header can lead
 * with the name and dim the directory below it.
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
 * Copies a diff file's name in the form the next step needs: the bare name for a search, the
 * repository path for Git commands and reviews, or the absolute path for a shell on the agent.
 * The menu closes on a pick, so the trigger itself shows that the copy landed.
 */
function GitDiffCopyPathMenu(props: { file: GitDiffListFile; name: string }) {
    const { isCopied, copy } = useCopyFeedback(props.file.path);

    const options = [
        { label: "Copy file name", value: props.name },
        { label: "Copy repository path", value: props.file.relativePath },
        { label: "Copy absolute path", value: props.file.path },
    ];

    return (
        <ActionMenu
            variant="icon"
            label="Copy path"
            triggerAriaLabel={
                isCopied ? "Copied!" : `Copy path of ${props.file.relativePath}`
            }
            closeAriaLabel="Close copy path menu"
            tooltip={isCopied ? "Copied!" : "Copy file name or path"}
            icon={
                isCopied ? (
                    <Check className="h-4 w-4" aria-hidden="true" />
                ) : (
                    <Copy className="h-4 w-4" aria-hidden="true" />
                )
            }
            className={cn("h-7 w-7", isCopied && "text-emerald-300")}
        >
            {(close) =>
                options.map((option) => (
                    <ActionMenuButton
                        key={option.label}
                        onClick={() => {
                            close();
                            void copy(option.value);
                        }}
                    >
                        {/* The value is shown so the reader knows which form each option copies. */}
                        <span className="flex min-w-0 flex-col">
                            <span>{option.label}</span>
                            <span className="break-all font-mono text-xs font-normal text-slate-500">
                                {option.value}
                            </span>
                        </span>
                    </ActionMenuButton>
                ))
            }
        </ActionMenu>
    );
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
    inline: boolean;
    onInlineChange: (inline: boolean) => void;
}) {
    const file = props.file;
    // Only a file with long lines needs to wrap, and the choice is only for this glance at
    // it, so it lives with the section instead of in the stored preferences.
    const [wrapLines, setWrapLines] = React.useState(false);
    const parts = splitRelativePath(file.relativePath);
    const bodyId = React.useId();

    return (
        <section
            aria-label={file.regionLabel}
            className="overflow-clip rounded-lg border border-slate-800 bg-slate-950/30"
        >
            <div
                className={cn(
                    "git-diff-sticky-header sticky z-10 flex min-w-0 items-center gap-3 bg-surface px-3 py-2",
                    !props.collapsed && "border-b border-slate-800",
                )}
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
                {/* The directory sits below the name, so on a phone it gets the full width
                    left of the controls instead of being cut off right after the name. */}
                <h3 className="flex min-w-0 flex-1 flex-col text-sm">
                    <Link
                        {...browserRoute(props.agent, file.path)}
                        search={file.fileSearch}
                        title={file.relativePath}
                        className="truncate font-medium text-slate-100 hover:text-blue-300 hover:underline"
                    >
                        {parts.name}
                    </Link>
                    {parts.directory === "" ? null : (
                        <span className="break-all font-mono text-xs text-slate-500">
                            {parts.directory}
                        </span>
                    )}
                    {file.originalPath === null ? null : (
                        <span className="break-all text-xs text-slate-500">
                            from {file.originalPath}
                        </span>
                    )}
                </h3>
                <GitDiffCopyPathMenu file={file} name={parts.name} />
                {/* A collapsed file shows no patch, so it has no layout or wrapping to switch. */}
                {props.collapsed ? null : (
                    <div className="flex shrink-0 items-center gap-2">
                        <DiffLayoutSwitch
                            label={`Diff layout for ${file.relativePath}`}
                            iconOnly
                            inline={props.inline}
                            onChange={props.onInlineChange}
                        />
                        <WrapDiffLinesToggle
                            label={`Wrap diff lines for ${file.relativePath}`}
                            wrap={wrapLines}
                            onChange={setWrapLines}
                            // Matches the height of the icon-only layout switch beside it.
                            className="h-7 w-7"
                        />
                    </div>
                )}
            </div>
            {props.collapsed ? null : (
                <div
                    id={bodyId}
                    className="git-diff-list-body w-full min-w-0 overflow-x-hidden [&>p]:p-3"
                >
                    {file.renderBody({
                        mergeModifiedLines: props.inline,
                        wrapLines,
                    })}
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
    /** The list's identity for remembering each file's layout, such as one commit. */
    scope: string;
}) {
    const layouts = useGitDiffLayouts(props.scope);

    return (
        <div className="grid grid-cols-[minmax(0,1fr)] gap-4">
            {props.files.map((file) => (
                <GitDiffSection
                    key={file.path}
                    agent={props.agent}
                    file={file}
                    collapsed={props.collapsed.isCollapsed(file.path)}
                    onToggle={() => props.collapsed.toggle(file.path)}
                    inline={layouts.isInline(file.path)}
                    onInlineChange={(inline) =>
                        layouts.setInline(file.path, inline)
                    }
                />
            ))}
        </div>
    );
}
