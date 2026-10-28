import { useInfiniteQuery } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";
import {
    ArrowDownAZ,
    Clock,
    FolderOpen,
    GitBranch,
    History,
    Tag,
} from "lucide-react";
import type { Agent } from "#ui/api-client";
import { browserRoute } from "#ui/browser-route";
import { Button } from "#ui/components/button";
import { CopyableGitRef } from "#ui/components/copyable-git-ref";
import { IconLinkButton } from "#ui/components/icon-button";
import {
    SegmentedControl,
    type SegmentedControlOption,
} from "#ui/components/segmented-control";
import {
    AuthorAvatar,
    CommitAge,
    GitPanelBar,
    ListMessage,
    shortCommitId,
} from "#ui/components/browser/git-parts";
import {
    GIT_LIST_PAGE_SIZE,
    gitLogQueryOptions,
    gitRefsQueryOptions,
    type GitRefListKind,
} from "#ui/queries";
import { getErrorMessage } from "#ui/utils/error";
import type { GitCommitSummary } from "#bindings/GitCommitSummary";
import type { GitRef } from "#bindings/GitRef";
import type { GitRefSort } from "#bindings/GitRefSort";

const browserRouteApi = getRouteApi("/_app/agents/$deviceName/browser/$");

/** The orders a branch or tag list offers, recent first because it is the default. */
const REF_SORT_OPTIONS: ReadonlyArray<SegmentedControlOption<GitRefSort>> = [
    {
        value: "recent",
        label: "Recent",
        icon: Clock,
        tooltip: "Most recently committed first",
    },
    {
        value: "name",
        label: "Name",
        icon: ArrowDownAZ,
        tooltip: "By name, with version numbers in numeric order",
    },
];

/** Asks for the next page of a list, which only exists while the agent reports more rows. */
function LoadMore(props: {
    label: string;
    /** Says how far the list has come, such as "Showing 100 of 345". */
    progress: string;
    hasNextPage: boolean;
    isFetchingNextPage: boolean;
    onLoadMore: () => void;
}) {
    if (!props.hasNextPage) {
        return null;
    }

    return (
        <div className="flex flex-col items-center gap-2 px-4 pb-5 sm:px-5">
            <Button
                type="button"
                size="sm"
                variant="secondary"
                tooltip={`Load the next ${GIT_LIST_PAGE_SIZE} rows`}
                disabled={props.isFetchingNextPage}
                onClick={props.onLoadMore}
            >
                {props.isFetchingNextPage ? "Loading..." : props.label}
            </Button>
            <p className="text-xs text-slate-500">{props.progress}</p>
        </div>
    );
}

/**
 * Groups consecutive commits by the local day they were authored, keeping the walk's order.
 * The walk is ordered by commit time, so a rebased or cherry-picked commit can bring a day
 * back after another one; each group is therefore identified by its first commit, not its day.
 */
function groupByDay(commits: GitCommitSummary[]) {
    const groups: Array<{
        id: string;
        day: string;
        commits: GitCommitSummary[];
    }> = [];

    for (const commit of commits) {
        const day = new Date(commit.authored_at * 1000).toLocaleDateString(
            undefined,
            { year: "numeric", month: "short", day: "numeric" },
        );

        const last = groups.at(-1);

        if (last?.day === day) {
            last.commits.push(commit);
        } else {
            groups.push({ id: commit.id, day, commits: [commit] });
        }
    }

    return groups;
}

/**
 * The commits that changed the browsed path, newest first and grouped by day like a
 * timeline. Each opens the same path at that commit's changes, so stepping through
 * history keeps the scope the user chose.
 */
export function GitCommitList(props: {
    agent: Agent;
    path: string;
    /** The browsed commit history starts at, or null for HEAD. */
    commitId: string | null;
    /**
     * HEAD's commit when the worktree is browsed. History is read from this id rather than
     * from HEAD, so every page walks the same history even if HEAD moves between pages.
     */
    headCommitId: string | null;
    /**
     * The worktree's checked-out branch, so the header can say whose history this is
     * instead of an opaque HEAD. Null for a detached HEAD or a browsed revision.
     */
    branchName: string | null;
}) {
    const query = useInfiniteQuery(
        gitLogQueryOptions(
            props.agent,
            props.path,
            props.commitId ?? props.headCommitId,
        ),
    );

    if (query.isPending) {
        return <ListMessage role="status">Loading commits...</ListMessage>;
    }

    if (query.isError) {
        return (
            <ListMessage role="alert">
                {getErrorMessage(query.error, "Failed to load commits")}
            </ListMessage>
        );
    }

    const relativePath = query.data.pages[0]?.repository_relative_path ?? "";
    const commits = query.data.pages.flatMap((page) => page.commits);
    const now = Date.now();

    // The walk only follows the starting commit's ancestors, so name the branch when there
    // is one; otherwise the commit itself is the most precise description.
    const startCommitId = props.commitId ?? props.headCommitId;
    const branchName = props.commitId === null ? props.branchName : null;

    return (
        <>
            <GitPanelBar>
                <span className="inline-flex items-center gap-2 font-medium text-slate-100">
                    <History
                        className="h-4 w-4 text-slate-400"
                        aria-hidden="true"
                    />
                    {branchName === null ? "History from" : "History of"}{" "}
                    {branchName !== null ? (
                        <CopyableGitRef kind="branch" value={branchName} />
                    ) : startCommitId !== null ? (
                        <CopyableGitRef kind="commit" value={startCommitId} />
                    ) : (
                        <code className="font-mono">HEAD</code>
                    )}
                </span>
                <span className="text-slate-400">
                    {relativePath === "" ? (
                        branchName === null ? (
                            "This commit and its ancestors."
                        ) : (
                            "Commits reachable from this branch."
                        )
                    ) : (
                        <>
                            Only commits that changed{" "}
                            <code className="font-mono text-slate-200">
                                {relativePath}
                            </code>
                            .
                        </>
                    )}
                </span>
            </GitPanelBar>
            {commits.length === 0 ? (
                <ListMessage role="status">
                    {relativePath === ""
                        ? "This repository has no commits yet."
                        : "No commits changed this path."}
                </ListMessage>
            ) : (
                <div
                    role="group"
                    aria-label="Commit history"
                    className="grid grid-cols-[minmax(0,1fr)] gap-5 px-4 py-4 sm:px-5"
                >
                    {groupByDay(commits).map((group) => (
                        <section key={group.id} aria-label={group.day}>
                            <h3 className="mb-2 text-xs font-semibold text-slate-400">
                                {group.day}
                            </h3>
                            <ol className="divide-y divide-slate-800 overflow-hidden rounded-lg border border-slate-800">
                                {group.commits.map((commit) => (
                                    <CommitRow
                                        key={commit.id}
                                        agent={props.agent}
                                        path={props.path}
                                        commit={commit}
                                        current={commit.id === props.commitId}
                                        now={now}
                                    />
                                ))}
                            </ol>
                        </section>
                    ))}
                </div>
            )}
            <LoadMore
                label="Load more commits"
                progress={`Showing ${commits.length} commits`}
                hasNextPage={query.hasNextPage}
                isFetchingNextPage={query.isFetchingNextPage}
                onLoadMore={() => void query.fetchNextPage()}
            />
        </>
    );
}

/**
 * One commit of the history. Its message and id open what it changed below the browsed
 * path, and the folder opens the path's files at that commit.
 */
function CommitRow(props: {
    agent: Agent;
    path: string;
    commit: GitCommitSummary;
    /** True for the commit the page is browsing, so the list shows where the user is. */
    current: boolean;
    now: number;
}) {
    const commit = props.commit;
    const shortId = shortCommitId(commit.id);

    return (
        <li
            aria-current={props.current ? "true" : undefined}
            className={`flex min-w-0 items-center gap-3 px-3 py-2.5 transition-colors ${props.current ? "bg-blue-400/10" : "hover:bg-tint"}`}
        >
            <AuthorAvatar name={commit.author_name} />
            <div className="min-w-0 flex-1">
                <Link
                    {...browserRoute(props.agent, props.path)}
                    search={{ view: "git", rev: commit.id }}
                    className="block truncate text-sm font-medium text-slate-100 hover:text-blue-300 hover:underline"
                    title={commit.summary}
                >
                    {commit.summary || "(no message)"}
                </Link>
                <p className="mt-0.5 truncate text-xs text-slate-500">
                    <span className="text-slate-400">{commit.author_name}</span>{" "}
                    committed{" "}
                    <CommitAge
                        authoredAt={commit.authored_at}
                        now={props.now}
                    />
                </p>
            </div>
            <span className="flex shrink-0 items-center gap-1.5">
                {props.current ? (
                    <span className="rounded-full border border-blue-400/40 px-2 py-0.5 text-[11px] font-medium text-blue-300">
                        Viewing
                    </span>
                ) : null}
                {commit.parent_ids.length > 1 ? (
                    <span className="rounded-full border border-slate-700 px-2 py-0.5 text-[11px] text-slate-400">
                        Merge
                    </span>
                ) : null}
                <IconLinkButton
                    {...browserRoute(props.agent, props.path)}
                    search={{ rev: commit.id }}
                    label={`Browse files at ${shortId}`}
                    variant="ghost"
                    size="sm"
                >
                    <FolderOpen className="h-4 w-4" aria-hidden="true" />
                </IconLinkButton>
                <CopyableGitRef
                    kind="commit"
                    value={commit.id}
                    className="text-slate-400"
                >
                    <Link
                        {...browserRoute(props.agent, props.path)}
                        search={{ view: "git", rev: commit.id }}
                        aria-label={`Changes of commit ${shortId}`}
                        className="rounded-md border border-slate-700 bg-slate-900/60 px-2 py-0.5 font-mono text-xs text-slate-300 hover:border-blue-400/50 hover:text-blue-300"
                    >
                        {shortId.slice(0, 7)}
                    </Link>
                </CopyableGitRef>
            </span>
        </li>
    );
}

/**
 * A repository's branches or tags, most recently committed first or by name. A ref's name
 * opens the browsed path at that ref, and its commit id opens what the commit changed there.
 * The order is the `refSort` search key, so it survives reload and Back and is shared by
 * both lists.
 */
export function GitRefList(props: {
    agent: Agent;
    path: string;
    /** The repository the page resolved, whose refs are listed. */
    repositoryRoot: string;
    kind: GitRefListKind;
}) {
    const navigate = browserRouteApi.useNavigate();
    const sort = browserRouteApi.useSearch().refSort ?? "recent";

    const query = useInfiniteQuery(
        gitRefsQueryOptions(props.agent, props.repositoryRoot, {
            kind: props.kind,
            sort,
        }),
    );

    const isBranches = props.kind === "branches";
    const noun = isBranches ? "branches" : "tags";

    // The control stays put while a new order loads, so the user can switch straight back.
    const sortControl = (
        <SegmentedControl
            label={`Sort ${noun}`}
            options={REF_SORT_OPTIONS}
            value={sort}
            onChange={(value) =>
                void navigate({
                    search: (previous) => ({
                        ...previous,
                        refSort: value === "recent" ? undefined : value,
                    }),
                    replace: true,
                })
            }
            // A fixed width, because the control is a size container that cannot grow to fit
            // its segments, and one wide enough to show their icons.
            className="w-60 max-w-full"
        />
    );

    if (query.isPending) {
        return (
            <>
                <GitPanelBar actions={sortControl}>{null}</GitPanelBar>
                <ListMessage role="status">{`Loading ${noun}...`}</ListMessage>
            </>
        );
    }

    if (query.isError) {
        return (
            <>
                <GitPanelBar actions={sortControl}>{null}</GitPanelBar>
                <ListMessage role="alert">
                    {getErrorMessage(query.error, `Failed to load ${noun}`)}
                </ListMessage>
            </>
        );
    }

    const refs = query.data.pages.flatMap((page) => page.refs);
    const total = query.data.pages.at(-1)?.total ?? refs.length;
    const now = Date.now();
    const Icon = isBranches ? GitBranch : Tag;

    return (
        <>
            <GitPanelBar actions={sortControl}>
                <span className="inline-flex items-center gap-2 font-medium text-slate-100">
                    <Icon
                        className="h-4 w-4 text-slate-400"
                        aria-hidden="true"
                    />
                    {total === 1
                        ? `1 ${isBranches ? "branch" : "tag"}`
                        : `${total} ${noun}`}
                </span>
                <span className="text-slate-400">
                    {sort === "name"
                        ? isBranches
                            ? "Default branch first, then by name, local before remote-tracking."
                            : "By name."
                        : isBranches
                          ? "Default branch first, then most recently committed, including remote-tracking branches."
                          : "Most recently committed first."}
                </span>
            </GitPanelBar>
            {refs.length === 0 ? (
                <ListMessage role="status">
                    {`This repository has no ${noun}.`}
                </ListMessage>
            ) : (
                <div className="px-4 py-4 sm:px-5">
                    <ul
                        aria-label={isBranches ? "Branches" : "Tags"}
                        className="divide-y divide-slate-800 overflow-hidden rounded-lg border border-slate-800"
                    >
                        {refs.map((ref) => (
                            <RefRow
                                key={ref.full_name}
                                agent={props.agent}
                                path={props.path}
                                gitRef={ref}
                                now={now}
                            />
                        ))}
                    </ul>
                </div>
            )}
            <LoadMore
                label={`Load more ${noun}`}
                progress={`Showing ${refs.length} of ${total}`}
                hasNextPage={query.hasNextPage}
                isFetchingNextPage={query.isFetchingNextPage}
                onLoadMore={() => void query.fetchNextPage()}
            />
        </>
    );
}

/** One branch or tag. It links by full ref name, which a tag and a branch cannot share. */
function RefRow(props: {
    agent: Agent;
    path: string;
    gitRef: GitRef;
    now: number;
}) {
    const ref = props.gitRef;
    const commit = ref.commit;
    const shortId = shortCommitId(commit.id);
    const Icon = ref.kind === "tag" ? Tag : GitBranch;

    return (
        <li
            className={`flex min-w-0 items-center gap-3 px-3 py-2.5 transition-colors hover:bg-tint ${ref.is_head ? "bg-emerald-400/5" : ""}`}
        >
            <span
                className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-full ${ref.is_head ? "bg-emerald-400/15 text-emerald-300" : "bg-slate-800 text-slate-400"}`}
            >
                <Icon className="h-3.5 w-3.5" aria-hidden="true" />
            </span>
            <div className="min-w-0 flex-1">
                <span className="flex min-w-0 items-center gap-2">
                    <CopyableGitRef
                        kind={ref.kind === "tag" ? "tag" : "branch"}
                        value={ref.name}
                        className="text-slate-400"
                    >
                        <Link
                            {...browserRoute(props.agent, props.path)}
                            search={{ rev: ref.full_name }}
                            className="truncate font-mono text-sm font-medium text-slate-100 hover:text-blue-300 hover:underline"
                            title={`Browse ${ref.full_name}`}
                        >
                            {ref.name}
                        </Link>
                    </CopyableGitRef>
                    {ref.is_head ? (
                        <span className="shrink-0 rounded-full border border-emerald-400/40 px-2 py-0.5 text-[11px] font-medium text-emerald-300">
                            HEAD
                        </span>
                    ) : null}
                    {ref.is_default ? (
                        <span className="shrink-0 rounded-full border border-blue-400/40 px-2 py-0.5 text-[11px] font-medium text-blue-300">
                            Default
                        </span>
                    ) : null}
                    {ref.kind === "remote_branch" ? (
                        <span className="shrink-0 rounded-full border border-slate-700 px-2 py-0.5 text-[11px] text-slate-400">
                            Remote
                        </span>
                    ) : null}
                </span>
                <p className="mt-0.5 truncate text-xs text-slate-500">
                    <span title={commit.summary} className="text-slate-400">
                        {commit.summary || "(no message)"}
                    </span>{" "}
                    · {commit.author_name} ·{" "}
                    <CommitAge
                        authoredAt={commit.authored_at}
                        now={props.now}
                    />
                </p>
            </div>
            <CopyableGitRef
                kind="commit"
                value={commit.id}
                className="shrink-0 text-slate-400"
            >
                <Link
                    {...browserRoute(props.agent, props.path)}
                    search={{ view: "git", rev: commit.id }}
                    aria-label={`Changes of commit ${shortId}`}
                    className="rounded-md border border-slate-700 bg-slate-900/60 px-2 py-0.5 font-mono text-xs text-slate-300 hover:border-blue-400/50 hover:text-blue-300"
                >
                    {shortId.slice(0, 7)}
                </Link>
            </CopyableGitRef>
        </li>
    );
}
