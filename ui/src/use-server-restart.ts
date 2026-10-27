import { getRouteApi } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";

import {
    invalidateAllAgentData,
    queryKeys,
    serverInfoQueryOptions,
} from "#ui/queries";
import { waitForRestart } from "#ui/utils/wait-for-restart";

const rootRouteApi = getRouteApi("__root__");

/**
 * Lets the restart and update dialogs wait for the replacement server process. Success only
 * counts after the old process was seen gone, because the old server keeps answering while
 * it shuts down; the fresh server info then carries the new version into the cache.
 */
export function useWaitForServerRestart(): () => Promise<void> {
    const { api } = rootRouteApi.useRouteContext();
    const queryClient = useQueryClient();

    return () => {
        let oldServerClosed = false;

        return waitForRestart(async () => {
            try {
                await queryClient.fetchQuery({
                    ...serverInfoQueryOptions(api),
                    staleTime: 0,
                });
            } catch (error) {
                oldServerClosed = true;
                throw error;
            }

            if (!oldServerClosed) {
                throw new Error("Old server is still shutting down");
            }

            // A new server process may report different agents and files.
            await Promise.all([
                queryClient.invalidateQueries({
                    queryKey: queryKeys.agents(),
                    exact: true,
                }),
                invalidateAllAgentData(queryClient),
            ]);
        }, "Server did not come back after restart");
    };
}
