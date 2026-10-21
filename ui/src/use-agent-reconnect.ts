import * as React from "react";
import { getRouteApi } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";

import type { AgentInfoResponse } from "#bindings/AgentInfoResponse";
import type { Agent } from "#ui/api-client";
import type { BinaryIdentity } from "#bindings/BinaryIdentity";
import type { ServerInfoResponse } from "#bindings/ServerInfoResponse";
import { agentsQueryOptions, invalidateAgentData } from "#ui/queries";
import { waitForRestart } from "#ui/utils/wait-for-restart";

const rootRouteApi = getRouteApi("__root__");

/** Proves a force-installed agent loaded the exact executable running the server. */
function matchesServerIdentity(
    binary: BinaryIdentity,
    server: ServerInfoResponse,
): boolean {
    return (
        binary.git_rev === server.git_rev &&
        binary.git_dirty === server.git_dirty &&
        binary.version_dirty === server.version_dirty &&
        binary.build_mode === server.build_mode &&
        binary.build_date === server.build_date
    );
}

/** Names what a returning agent must prove beyond being connected on a new connection. */
type ReconnectCheck = {
    /** Chooses the connected entry that counts as the agent's replacement process. */
    isReplacement: (info: AgentInfoResponse) => boolean;
    /** Rejects a replacement that is not the expected build yet. */
    verify: (info: AgentInfoResponse) => void;
    stillWaitingMessage: string;
    timeoutMessage: string;
};

/**
 * Lets a restart or upgrade button wait for the agent to come back. A restarted agent shows
 * up as the same id on a new connection, so the wait fetches the inventory fresh until that
 * appears, then refreshes the agent's cached data. Every place that restarts or upgrades an
 * agent shares this instead of carrying its own copy of the probe.
 */
export function useAgentReconnect(agent: Agent) {
    const queryClient = useQueryClient();
    const { api } = rootRouteApi.useRouteContext();

    /** Polls until the inventory lists the replacement process and it passes verification. */
    const waitUntilBack = React.useCallback(
        (check: ReconnectCheck) =>
            waitForRestart(async () => {
                const infos = await queryClient.fetchQuery({
                    ...agentsQueryOptions(api),
                    staleTime: 0,
                });

                const replacement = infos.find(
                    (info) =>
                        info.id === agent.id &&
                        info.status === "connected" &&
                        info.connection_id !== agent.connectionId &&
                        check.isReplacement(info),
                );

                if (!replacement) {
                    throw new Error(check.stillWaitingMessage);
                }

                check.verify(replacement);
                await invalidateAgentData(queryClient, agent.id);
            }, check.timeoutMessage),
        [agent.connectionId, agent.id, api, queryClient],
    );

    /** Waits for the same agent to reconnect after it re-executed itself. */
    const waitForAgentRestart = React.useCallback(
        () =>
            waitUntilBack({
                isReplacement: () => true,
                verify: () => {},
                stillWaitingMessage: "Agent is still restarting",
                timeoutMessage: "Agent did not come back after restart",
            }),
        [waitUntilBack],
    );

    /**
     * Waits for the agent to reconnect running the target version. When the server's own
     * binary was force-installed, the agent must also match that build exactly.
     */
    const waitForAgentUpgrade = React.useCallback(
        (
            targetVersion: string,
            requiredServerBuild: ServerInfoResponse | null,
        ) =>
            waitUntilBack({
                isReplacement: (info) => info.binary?.version === targetVersion,
                verify: (info) => {
                    if (!info.binary) {
                        throw new Error("Agent is still upgrading");
                    }

                    if (
                        requiredServerBuild !== null &&
                        !matchesServerIdentity(info.binary, requiredServerBuild)
                    ) {
                        throw new Error(
                            "Agent has not loaded the running server binary",
                        );
                    }
                },
                stillWaitingMessage: "Agent is still upgrading",
                timeoutMessage: "Agent did not come back after upgrade",
            }),
        [waitUntilBack],
    );

    return { waitForAgentRestart, waitForAgentUpgrade };
}
