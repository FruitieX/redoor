import { useMatch, useMatchRoute } from "@tanstack/react-router";

import type { Agent } from "#ui/api-client";
import { useAgents } from "#ui/use-agents";

/** Route id of the layout that owns every page of one agent. */
const agentRouteId = "/_app/agents/$agentId";

/**
 * Reads the agent id from the committed agent route's params, so it always agrees with the
 * other data read from the committed matches, such as the browser location. Use it for what
 * describes the page that is on screen.
 */
export function useDisplayedAgentId(): string | null {
    const agentId = useMatch({
        from: agentRouteId,
        shouldThrow: false,
        select: (match) => match.params.agentId,
    });

    return agentId ?? null;
}

/**
 * Reads the agent id the user is heading to, including a navigation whose loaders are still
 * running. Use it for controls that must follow a click at once, such as the sidebar
 * highlight and the terminal shortcut: a slow agent would otherwise leave them pointed at
 * the previous device for the length of its loader.
 */
export function useTargetAgentId(): string | null {
    const matchRoute = useMatchRoute();

    // The router only exposes a pending location through `pending: true`, which is false
    // while idle, so ask for it first and fall back to the settled location.
    const params =
        matchRoute({ to: "/agents/$agentId", fuzzy: true, pending: true }) ||
        matchRoute({ to: "/agents/$agentId", fuzzy: true });

    return params ? params.agentId : null;
}

/**
 * Resolves the agent the user is heading to, or undefined on application pages and while the
 * agent is absent from the inventory, such as between a rename and its redirect.
 */
export function useTargetAgent(): Agent | undefined {
    const agentId = useTargetAgentId();
    const agents = useAgents();

    return agentId === null
        ? undefined
        : agents.find((agent) => agent.id === agentId);
}
