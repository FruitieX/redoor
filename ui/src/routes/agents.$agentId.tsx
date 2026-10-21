import { createFileRoute, Outlet } from "@tanstack/react-router";
import * as React from "react";
import { useSetAtom } from "jotai";
import { rememberProvisioningStatusAtom } from "#ui/provisioning-status";
import { z } from "zod";

import type { CaseSensitivity } from "#bindings/CaseSensitivity";
import { SearchDialog } from "#ui/components/search-dialog";
import { RouteError } from "#ui/components/route-error";
import {
    agentDetailsQueryOptions,
    fetchQueryUncancelled,
    loadAgent,
} from "#ui/queries";
import { useAgent } from "#ui/use-agents";

export type AgentSearch = {
    q?: string;
    mode?: "content";
    context?: number;
    timeout?: number;
    hidden?: boolean;
    gitignore?: boolean;
    regex?: boolean;
    gitroot?: boolean;
    case?: CaseSensitivity;
};

/** Restricts grep timeout URLs to the same safe range accepted by recursive search. */
const searchTimeoutSchema = z.coerce
    .number()
    .int()
    .min(1)
    .max(60)
    .optional()
    .catch(undefined);

/** Restricts shared before/after grep context to the limit accepted by the agent. */
const searchContextSchema = z.coerce
    .number()
    .int()
    .min(0)
    .max(20)
    .optional()
    .catch(undefined);

/** Accepts booleans from both router navigation and directly entered query strings. */
const optionalBooleanSchema = z
    .union([
        z.boolean(),
        z.literal("true").transform(() => true),
        z.literal("false").transform(() => false),
    ])
    .optional()
    .catch(undefined);

/** Keeps presence meaningful because an empty q still opens and focuses the dialog. */
const searchQuerySchema = z.string().optional().catch(undefined);

/** Treats a missing mode as path search while rejecting stale shared URL values. */
const searchModeSchema = z.literal("content").optional().catch(undefined);

/** Rejects stale case values while keeping a missing value equivalent to smart case. */
const caseSensitivitySchema = z
    .enum(["smart", "sensitive", "insensitive"])
    .optional()
    .catch(undefined);

export const Route = createFileRoute("/agents/$agentId")({
    validateSearch: (search): AgentSearch => ({
        q: searchQuerySchema.parse(search.q),
        mode: searchModeSchema.parse(search.mode),
        context: searchContextSchema.parse(search.context),
        timeout: searchTimeoutSchema.parse(search.timeout),
        hidden: optionalBooleanSchema.parse(search.hidden),
        gitignore: optionalBooleanSchema.parse(search.gitignore),
        regex: optionalBooleanSchema.parse(search.regex),
        gitroot: optionalBooleanSchema.parse(search.gitroot),
        case: caseSensitivitySchema.parse(search.case),
    }),
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

    const rememberProvisioningStatus = useSetAtom(
        rememberProvisioningStatusAtom,
    );

    const agentName = agent?.name;
    const provisioningStatus = agent?.provisioningStatus;

    React.useEffect(() => {
        if (agentName === undefined || !provisioningStatus?.length) {
            return;
        }

        rememberProvisioningStatus({
            agentName,
            messages: provisioningStatus,
        });
    }, [agentName, provisioningStatus, rememberProvisioningStatus]);

    return (
        <>
            <Outlet />
            {agent ? <SearchDialog agent={agent} /> : null}
        </>
    );
}
