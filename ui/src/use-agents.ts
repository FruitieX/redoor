import { useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi } from "@tanstack/react-router";
import * as React from "react";

import type { AgentInfoResponse } from "#bindings/AgentInfoResponse";
import type { Agent } from "#ui/api-client";
import { agentsQueryOptions } from "#ui/queries";

const rootRouteApi = getRouteApi("__root__");

/**
 * Reads the inventory from the query cache and wraps each plain snapshot in its request
 * client. The cache holds plain data so structural sharing keeps unchanged agents
 * identical across refetches, and `ApiClient.agentFor` memoizes per snapshot, so an agent's
 * identity only changes when its data does. The root loader primes the cache, so this
 * never suspends in the authenticated shell.
 */
export function useAgents(): Agent[] {
    const { api } = rootRouteApi.useRouteContext();

    const select = React.useMemo(
        () => (infos: AgentInfoResponse[]) =>
            infos.map((info) => api.agentFor(info)),
        [api],
    );

    return useSuspenseQuery({ ...agentsQueryOptions(api), select }).data;
}

/**
 * Selects one agent without rerendering for changes to other agents. Returns undefined
 * while the agent is absent, such as for the moment between a rename and the navigation to
 * its new id, so callers decide whether that is an error.
 */
export function useAgent(agentId: string): Agent | undefined {
    const { api } = rootRouteApi.useRouteContext();

    const select = React.useCallback(
        (infos: AgentInfoResponse[]) => {
            const info = infos.find((entry) => entry.id === agentId);

            return info ? api.agentFor(info) : undefined;
        },
        [api, agentId],
    );

    return useSuspenseQuery({ ...agentsQueryOptions(api), select }).data;
}

/** Throws when the agent is gone so the route's error boundary reports it as missing. */
export function useRequiredAgent(agentId: string): Agent {
    const agent = useAgent(agentId);

    if (!agent) {
        throw new Error(`Agent not found: ${agentId}`);
    }

    return agent;
}
