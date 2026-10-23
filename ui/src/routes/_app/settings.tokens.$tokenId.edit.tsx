import * as React from "react";
import { createFileRoute, useRouter } from "@tanstack/react-router";
import {
    useMutation,
    useQueryClient,
    useSuspenseQuery,
} from "@tanstack/react-query";
import { ArrowLeft, KeyRound } from "lucide-react";

import type { AccessTokenResponse } from "#bindings/AccessTokenResponse";
import { accessTokensQueryOptions, queryKeys } from "#ui/queries";
import { useAgents } from "#ui/use-agents";
import {
    AccessTokenPolicyFields,
    isExpired,
    useAccessTokenPolicyDraft,
} from "#ui/components/access-token-policy";
import { Button } from "#ui/components/button";
import { LinkButton } from "#ui/components/link-button";
import { Tooltip } from "#ui/components/tooltip";
import { getErrorMessage } from "#ui/utils/error";

export const Route = createFileRoute("/_app/settings/tokens/$tokenId/edit")({
    loader: async ({ context }) => {
        // There is no single-token read endpoint; the list is small and already shared with the
        // token overview, so the page picks its token out of that cache entry.
        await context.queryClient.ensureQueryData(
            accessTokensQueryOptions(context.api),
        );
    },
    component: EditAccessTokenPage,
});

/** Resolves the token from the shared list so a deleted or unknown id gets a clear message. */
function EditAccessTokenPage() {
    const { api } = Route.useRouteContext();
    const { tokenId } = Route.useParams();
    const { data } = useSuspenseQuery(accessTokensQueryOptions(api));
    const token = data.tokens.find((item) => item.id === tokenId) ?? null;

    return (
        <div className="min-h-full bg-canvas px-4 py-8 sm:px-8">
            <div className="mx-auto max-w-4xl">
                <LinkButton
                    to="/settings/tokens"
                    variant="ghost"
                    size="sm"
                    className="mb-4"
                >
                    <ArrowLeft className="h-4 w-4" />
                    Back to access tokens
                </LinkButton>
                {token === null ? (
                    <p
                        role="alert"
                        className="rounded-xl border border-slate-800 bg-surface p-6 text-slate-300"
                    >
                        This access token no longer exists. It may have been
                        deleted.
                    </p>
                ) : (
                    // Keyed by id so moving between tokens never carries one draft into another.
                    <EditAccessTokenForm key={token.id} token={token} />
                )}
            </div>
        </div>
    );
}

/** Edits the policy only; the secret stays unchanged, so clients keep working after saving. */
function EditAccessTokenForm(props: { token: AccessTokenResponse }) {
    const { api } = Route.useRouteContext();
    const queryClient = useQueryClient();
    const router = useRouter();
    const agents = useAgents();
    const draft = useAccessTokenPolicyDraft(props.token);
    const headingId = React.useId();
    const expired = isExpired(props.token);

    const mutation = useMutation({
        mutationFn: async () => {
            await api.updateAccessToken(props.token.id, draft.policy);
            await queryClient.invalidateQueries({
                queryKey: queryKeys.accessTokens(),
            });
            await router.navigate({
                to: "/settings/tokens",
                search: { notice: "updated" },
            });
        },
    });

    /** Native submit keeps Enter in the name field working like the Save button. */
    const submit = (event: React.FormEvent<HTMLFormElement>) => {
        event.preventDefault();

        if (draft.canSubmit && !expired) mutation.mutate();
    };

    return (
        <main className="overflow-hidden rounded-xl border border-slate-800 bg-surface shadow-2xl shadow-black/20">
            <div className="border-b border-slate-800 bg-gradient-to-r from-blue-500/10 via-transparent to-transparent px-6 py-6 sm:px-8">
                <div className="flex items-center gap-3">
                    <div className="rounded-lg border border-blue-500/30 bg-blue-500/10 p-2 text-blue-300">
                        <KeyRound className="h-5 w-5" aria-hidden="true" />
                    </div>
                    <div className="min-w-0">
                        <h1
                            id={headingId}
                            className="text-2xl font-bold text-slate-100"
                        >
                            Edit access token
                        </h1>
                        <p className="mt-1 text-sm text-slate-400">
                            Changes apply to future requests using the same
                            secret. Choose the least access this client needs.
                        </p>
                    </div>
                </div>
            </div>
            <form
                aria-labelledby={headingId}
                onSubmit={submit}
                className="space-y-7 p-6 sm:p-8"
            >
                {expired ? (
                    <p
                        role="alert"
                        className="rounded-md border border-slate-700 bg-slate-800/50 px-3 py-2 text-sm text-slate-300"
                    >
                        This token has expired and can no longer be edited.
                        Delete it and create a new one instead.
                    </p>
                ) : null}
                <AccessTokenPolicyFields
                    draft={draft}
                    agents={agents}
                    disabled={mutation.isPending || expired}
                />
                {mutation.isError ? (
                    <p
                        role="alert"
                        className="rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-sm text-red-300"
                    >
                        {getErrorMessage(
                            mutation.error,
                            "Could not save the token",
                        )}
                    </p>
                ) : null}
                <div className="flex flex-col-reverse gap-3 border-t border-slate-800 pt-6 sm:flex-row sm:justify-end">
                    <Tooltip content="Discard changes and return to the token list">
                        <LinkButton to="/settings/tokens" variant="secondary">
                            Cancel
                        </LinkButton>
                    </Tooltip>
                    <Button
                        tooltip={
                            draft.canSubmit
                                ? "Save the token policy"
                                : "Enter a name, at least one permission, and at least one device"
                        }
                        type="submit"
                        isLoading={mutation.isPending}
                        disabled={!draft.canSubmit || expired}
                    >
                        Save changes
                    </Button>
                </div>
            </form>
        </main>
    );
}
