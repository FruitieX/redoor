import * as React from "react";
import { useNavigate } from "@tanstack/react-router";

import type { AgentSearch } from "#ui/agent-search";

/**
 * Changes only the search workflow's keys on the current URL, so the path, the hash and the
 * child view's own parameters (view, line, preview) survive. A key set to undefined is
 * removed. Going through the router's typed `search` updater means every change passes the
 * agent route's validation instead of being serialized by hand.
 */
export function useAgentSearchNavigation() {
    const navigate = useNavigate();

    /** Replaces the history entry by default because search state is a view of the same page. */
    const updateSearch = React.useCallback(
        (changes: AgentSearch, replace = true) =>
            navigate({
                to: ".",
                hash: true,
                replace,
                search: (previous) => ({ ...previous, ...changes }),
            }),
        [navigate],
    );

    /** Opens the empty path-search dialog from any toolbar or shortcut. */
    const openSearch = React.useCallback(
        () => updateSearch({ q: "", mode: undefined }),
        [updateSearch],
    );

    return { updateSearch, openSearch };
}
