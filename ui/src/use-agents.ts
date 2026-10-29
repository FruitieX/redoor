import { useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi, notFound } from "@tanstack/react-router";
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
 * Selects one agent by id without rerendering for changes to other agents. Returns undefined
 * while the agent is absent, such as after it was removed, so callers decide whether that is
 * an error. A rename keeps the id, so it never makes the agent absent.
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

/** Throws `notFound()` when the agent is gone, so the device route reports it as missing. */
export function useRequiredAgent(agentId: string): Agent {
    const agent = useAgent(agentId);

    if (!agent) {
        throw notFound();
    }

    return agent;
}
