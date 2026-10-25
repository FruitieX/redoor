import { useMutation, useSuspenseQuery } from "@tanstack/react-query";
import { deviceRoute } from "#ui/browser-route";
import { createFileRoute, useRouter } from "@tanstack/react-router";

import {
    ManagedAgentForm,
    type ManagedAgentSubmitRequest,
} from "#ui/components/managed-agent-form";
import { agentsQueryOptions, serverInfoQueryOptions } from "#ui/queries";
import { getErrorMessage } from "#ui/utils/error";

export const Route = createFileRoute("/_app/agents/new")({
    component: NewManagedAgentPage,
});

/** Persists a new managed local or SSH agent and opens its newly registered tab. */
function NewManagedAgentPage() {
    const { api, queryClient } = Route.useRouteContext();
    const { data: serverInfo } = useSuspenseQuery(serverInfoQueryOptions(api));
    const router = useRouter();

    const createMutation = useMutation({
        mutationFn: async (submission: ManagedAgentSubmitRequest) => {
            if (submission.kind === "local") {
                return api.createLocalAgent(submission.request);
            }

            return api.createSshAgent(submission.request);
        },
        onSuccess: async (response) => {
            await queryClient.invalidateQueries(agentsQueryOptions(api));
            await router.navigate({
                ...deviceRoute(response.agent, "/agents/$deviceName"),
            });
        },
    });

    const mutationError = createMutation.isError
        ? getErrorMessage(createMutation.error, "Failed to add device")
        : null;

    return (
        <ManagedAgentForm
            mode="add"
            configPath={serverInfo.config_path}
            isSubmitting={createMutation.isPending}
            mutationError={mutationError}
            onSubmit={(request) => createMutation.mutate(request)}
            onChange={() => createMutation.reset()}
        />
    );
}
