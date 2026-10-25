import { useMatch } from "@tanstack/react-router";

import { isLsDirectoryResponse } from "#ui/ls-response";

/**
 * What the shell needs to know about the filesystem location on screen. The browser route's
 * loader data is the only place that knows whether a path is a file, a directory or
 * unavailable, so this type is the one boundary where other components depend on that shape.
 */
export type BrowserLocation = {
    agentId: string;
    /** The path requested by the URL, which may be an alias of the resolved path. */
    path: string;
    /** What the path resolved to, or null for a missing path or an agent that is not connected. */
    entry: "directory" | "file" | null;
    /** The canonical path reported by the listing, or null when the path did not resolve. */
    resolvedPath: string | null;
    /** Whether the agent was connected when the route loaded, so repository discovery ran. */
    isConnected: boolean;
};

/**
 * Selects the browser route's location in one place instead of every consumer digging through
 * `useMatches`. Returns null outside the file browser, so callers fall back to the agent's own
 * cwd. The selection is structurally shared, so subscribers only rerender when it changes.
 */
export function useBrowserLocation(): BrowserLocation | null {
    const location = useMatch({
        from: "/_app/agents/$deviceName/browser/$",
        shouldThrow: false,
        select: (match): BrowserLocation | null => {
            const data = match.loaderData;

            if (!data) {
                return null;
            }

            const lsResult = data.lsResult;

            return {
                agentId: data.agentId,
                path: data.path,
                entry: lsResult
                    ? isLsDirectoryResponse(lsResult)
                        ? "directory"
                        : "file"
                    : null,
                resolvedPath: lsResult?.path ?? null,
                isConnected: data.kind === "connected",
            };
        },
    });

    return location ?? null;
}
