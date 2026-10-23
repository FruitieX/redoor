import { createFileRoute, Outlet } from "@tanstack/react-router";

import { agentSearchSchema } from "#ui/agent-search";
import { useRememberProvisioningStatus } from "#ui/provisioning-status";
import { SearchDialog } from "#ui/components/search-dialog";
import { RouteError } from "#ui/components/route-error";
import {
    agentDetailsQueryOptions,
    fetchQueryUncancelled,
    loadAgent,
} from "#ui/queries";
import { useAgent } from "#ui/use-agents";

export const Route = createFileRoute("/_app/agents/$agentId")({
    validateSearch: agentSearchSchema,
    loader: async ({ context, params }) => {
        const agent = await loadAgent(context, params.agentId);

        if (agent.status === "connected") {
            await fetchQueryUncancelled(
                context.queryClient,
                agentDetailsQueryOptions(agent),
            );
        }
    },
    component: AgentRouteLayout,
    errorComponent: RouteError,
});

/** Hosts the agent's child views and the search dialog that every one of them shares. */
function AgentRouteLayout() {
    const { agentId } = Route.useParams();
    // Tolerates a missing agent: a rename removes the old id from the inventory before the
    // edit page has navigated to the new one, and the page must stay up until it does.
    const agent = useAgent(agentId);

    useRememberProvisioningStatus(agent);

    return (
        <>
            <Outlet />
            {agent ? <SearchDialog agent={agent} /> : null}
        </>
    );
}
