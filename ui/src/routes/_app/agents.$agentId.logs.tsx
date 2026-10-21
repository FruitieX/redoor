import { createFileRoute } from "@tanstack/react-router";
import { AlertCircle, HardDrive } from "lucide-react";

import { LinkButton } from "#ui/components/link-button";
import { LogViewer } from "#ui/components/log-viewer";
import {
    agentLoggingLevelQueryOptions,
    loadAgent,
    queryKeys,
} from "#ui/queries";
import { useRequiredAgent } from "#ui/use-agents";

export const Route = createFileRoute("/_app/agents/$agentId/logs")({
    loader: async ({ params, context }) => {
        const agent = await loadAgent(context, params.agentId);

        if (agent.status === "connected") {
            await context.queryClient.ensureQueryData(
                agentLoggingLevelQueryOptions(agent),
            );
        }
    },
    component: AgentLogsPage,
});

/** Avoids opening a retrying live socket when retained inventory says the agent is disconnected. */
function AgentLogsPage() {
    const { agentId } = Route.useParams();
    const agent = useRequiredAgent(agentId);

    if (agent.status !== "connected") {
        return (
            <div className="flex h-full items-center justify-center p-8">
                <section className="max-w-lg text-center">
                    <AlertCircle className="mx-auto h-12 w-12 text-amber-400" />
                    <h1 className="mt-4 text-2xl font-semibold text-slate-100">
                        {agent.name} is disconnected
                    </h1>
                    <p className="mt-2 text-slate-400">
                        Start the Redoor agent on this device to view its live
                        logs.
                    </p>
                    <LinkButton
                        to="/agents/$agentId"
                        params={{ agentId: agent.id }}
                        className="mt-6"
                    >
                        <HardDrive className="h-4 w-4" />
                        Device
                    </LinkButton>
                </section>
            </div>
        );
    }

    return (
        <LogViewer
            title={`${agent.name} logs`}
            sourceLabel={agent.name}
            websocketUrl={agent.getLogsWebSocketUrl()}
            loggingLevelControl={{
                queryKey: queryKeys.agentLoggingLevel(agent.id),
                load: () => agent.getLoggingLevel(),
                update: (level) => agent.updateLoggingLevel(level),
            }}
        />
    );
}
