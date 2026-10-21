import * as React from "react";
import { useMutation, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, redirect, useRouter } from "@tanstack/react-router";
import { Trash2 } from "lucide-react";
import { Button } from "#ui/components/button";

import { ConfirmationDialog } from "#ui/components/confirmation-dialog";
import {
    ManagedAgentForm,
    type ManagedAgentKind,
    type ManagedAgentSubmitRequest,
} from "#ui/components/managed-agent-form";
import { Tooltip } from "#ui/components/tooltip";
import {
    agentConfigurationQueryOptions,
    agentsQueryOptions,
    fetchQueryUncancelled,
    loadAgent,
    queryKeys,
    serverInfoQueryOptions,
} from "#ui/queries";
import { useAgent } from "#ui/use-agents";
import { getErrorMessage } from "#ui/utils/error";

export const Route = createFileRoute("/_app/agents/$agentId/edit")({
    loader: async ({ context, params }) => {
        const agent = await loadAgent(context, params.agentId);

        if (!agent.configurationEditable) {
            throw redirect({ to: "/agents" });
        }

        const kind: ManagedAgentKind =
            agent.sshTarget === null ? "local" : "ssh";

        // The form's initial values must reflect the TOML as it is now, not an earlier visit.
        await fetchQueryUncancelled(context.queryClient, {
            ...agentConfigurationQueryOptions(
                context.api,
                params.agentId,
                kind,
            ),
            staleTime: 0,
        });

        return { kind };
    },
    component: EditManagedAgentPage,
});

/** Updates or permanently removes one stopped managed local or SSH agent. */
function EditManagedAgentPage() {
    const { api, queryClient } = Route.useRouteContext();
    const { agentId } = Route.useParams();
    const { kind } = Route.useLoaderData();
    const { data: serverInfo } = useSuspenseQuery(serverInfoQueryOptions(api));

    const { data: configuration } = useSuspenseQuery(
        agentConfigurationQueryOptions(api, agentId, kind),
    );

    // Absent for a moment after a rename, until the page follows to the new id.
    const agent = useAgent(agentId);
    const router = useRouter();
    const [isDeleteOpen, setIsDeleteOpen] = React.useState(false);
    const isRunning = agent?.status !== "stopped";

    const updateMutation = useMutation({
        mutationFn: async (submission: ManagedAgentSubmitRequest) => {
            const response =
                submission.kind === "local"
                    ? await api.updateLocalAgent(agentId, submission.request)
                    : await api.updateSshAgent(agentId, submission.request);

            await queryClient.invalidateQueries(agentsQueryOptions(api));

            // A renamed agent's settings are loaded under its new id by the navigation below.
            if (response.agent.id === agentId) {
                await queryClient.invalidateQueries({
                    queryKey: queryKeys.agentConfiguration(agentId),
                });
            }

            // A rename drops the old id from the inventory above; this page tolerates that
            // gap and moves to the new identity here.
            await router.navigate({
                to: "/agents/$agentId/edit",
                params: { agentId: response.agent.id },
            });

            return response;
        },
    });

    const deleteMutation = useMutation({
        mutationFn: () => api.deleteManagedAgent(agentId, configuration.kind),
        onSuccess: async () => {
            await queryClient.invalidateQueries(agentsQueryOptions(api));
            await router.navigate({ to: "/agents" });
        },
    });

    const mutationError = updateMutation.isError
        ? getErrorMessage(updateMutation.error, "Failed to update device")
        : null;

    const isBusy = updateMutation.isPending || deleteMutation.isPending;
    const kindLabel = configuration.kind === "local" ? "local" : "SSH";

    return (
        <>
            <ManagedAgentForm
                key={agentId}
                mode="edit"
                configuration={configuration}
                configPath={serverInfo.config_path}
                isSubmitting={updateMutation.isPending}
                isDisabled={isBusy}
                submitLabel={isRunning ? "Stop agent and save" : "Save device"}
                submittingLabel={
                    isRunning ? "Stopping and saving..." : "Saving device..."
                }
                submitDescription={
                    isRunning
                        ? "The agent must stop before its managed configuration can be changed. Saving will stop it automatically."
                        : undefined
                }
                submitTooltip={
                    isBusy
                        ? "Wait for the current save or delete to finish"
                        : isRunning
                          ? "Stop the agent and save the new configuration"
                          : `Save the managed ${kindLabel} configuration`
                }
                mutationError={mutationError}
                onSubmit={(request) => updateMutation.mutate(request)}
                onChange={() => updateMutation.reset()}
            >
                <div className="border-t border-red-950 pt-6">
                    <Tooltip
                        content={
                            isBusy
                                ? "Wait for the current save or delete to finish"
                                : isRunning
                                  ? "Stop the agent and remove this device from Redoor"
                                  : "Remove this device from Redoor"
                        }
                    >
                        <Button
                            type="button"
                            variant="danger"
                            onClick={() => setIsDeleteOpen(true)}
                            disabled={isBusy}
                            className="rounded-md disabled:opacity-60"
                        >
                            <Trash2 className="h-4 w-4" aria-hidden="true" />
                            Remove device
                        </Button>
                    </Tooltip>
                </div>
            </ManagedAgentForm>
            <ConfirmationDialog
                isOpen={isDeleteOpen}
                title={`Remove ${agentId} from Redoor?`}
                description="Stops its agent and removes the device from the TOML configuration. Files on the device are not deleted."
                confirmLabel="Remove device"
                busyLabel="Removing device..."
                isBusy={deleteMutation.isPending}
                errorMessage={
                    deleteMutation.isError
                        ? getErrorMessage(
                              deleteMutation.error,
                              "Failed to remove device",
                          )
                        : null
                }
                onClose={() => {
                    if (!deleteMutation.isPending) setIsDeleteOpen(false);
                }}
                onConfirm={() => deleteMutation.mutate()}
            />
        </>
    );
}
