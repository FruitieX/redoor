import { createFileRoute, Link } from "@tanstack/react-router";

import { LogViewer } from "#ui/components/log-viewer";
import { useAgents } from "#ui/use-agents";
import { sortAgentsByName } from "#ui/utils/agents";
import { queryKeys, serverLoggingLevelQueryOptions } from "#ui/queries";

export const Route = createFileRoute("/_app/logs")({
    loader: async ({ context }) => {
        await context.queryClient.ensureQueryData(
            serverLoggingLevelQueryOptions(context.api),
        );
    },
    component: ServerLogsPage,
});

/** Keeps server logs available while exposing deterministic links to connected agent streams. */
function ServerLogsPage() {
    const { api } = Route.useRouteContext();
    const agents = useAgents();

    const connectedAgents = sortAgentsByName(
        agents.filter((agent) => agent.status === "connected"),
    );

    const agentLinks = (
        <nav
            aria-label="Agent logs"
            className="flex flex-wrap items-center gap-2 text-sm"
        >
            <span className="text-slate-400">Agent logs:</span>
            {connectedAgents.length === 0 ? (
                <span className="text-slate-500">No connected agents</span>
            ) : (
                connectedAgents.map((agent) => (
                    <Link
                        key={agent.id}
                        to="/agents/$agentId/logs"
                        params={{ agentId: agent.id }}
                        aria-label={`View logs for ${agent.name}`}
                        className="rounded border border-slate-700 px-2 py-1 text-slate-200 hover:bg-tint"
                    >
                        {agent.name}
                    </Link>
                ))
            )}
        </nav>
    );

    return (
        <LogViewer
            title="Server logs"
            sourceLabel="Server"
            websocketUrl={api.getServerLogsWebSocketUrl()}
            headerActions={agentLinks}
            loggingLevelControl={{
                queryKey: queryKeys.serverLoggingLevel(),
                load: () => api.getServerLoggingLevel(),
                update: (level) => api.updateServerLoggingLevel(level),
            }}
        />
    );
}
