/** Primes the worktree Git view for the file browser route, apart from the route file. */
import type { Agent } from "#ui/api-client";
import type { BrowserSearch } from "#ui/browser-search";
import {
    fetchQueryUncancelled,
    gitDiffQueryOptions,
    gitStatusQueryOptions,
    prefetchGitList,
    type QueryContext,
} from "#ui/queries";
import type { GitContextResponse } from "#bindings/GitContextResponse";

/**
 * Loads what the worktree Git view is about to show: a list when a Git tab is open,
 * otherwise the directory's status or the file's diff. Status and diffs refetch on every
 * navigation because the worktree changes under the page.
 */
export async function prefetchWorktreeGitView(options: {
    context: QueryContext;
    agent: Agent;
    path: string;
    isDirectory: boolean;
    /** The path's repository and HEAD, from the Git context the view reads them from too. */
    gitContext: Extract<GitContextResponse, { status: "inside_worktree" }>;
    deps: Pick<BrowserSearch, "gitTab" | "diffMode">;
}) {
    const { context, agent, path, deps } = options;

    if (deps.gitTab !== undefined) {
        await prefetchGitList({
            queryClient: context.queryClient,
            agent,
            path,
            repositoryRoot: options.gitContext.repository_root,
            commitId: options.gitContext.head_commit_id,
            tab: deps.gitTab,
        });
    } else if (options.isDirectory) {
        await fetchQueryUncancelled(context.queryClient, {
            ...gitStatusQueryOptions(agent, path),
            staleTime: 0,
        });
    } else {
        await fetchQueryUncancelled(context.queryClient, {
            ...gitDiffQueryOptions(agent, [path], deps.diffMode ?? "full"),
            staleTime: 0,
        });
    }
}
