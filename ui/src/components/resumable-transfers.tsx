import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import type { ApiClient } from "#ui/api-client";
import type { PartialActionRequest } from "#bindings/PartialActionRequest";
import { partialsQueryOptions, queryKeys } from "#ui/queries";
import { Button } from "#ui/components/button";
import { Tooltip } from "#ui/components/tooltip";
import { formatSize } from "#ui/utils/path";

/** Durable inventory remains available even after transfer history or processes disappear. */
export function ResumableTransfers(props: { api: ApiClient }) {
    const client = useQueryClient();
    const inventory = useQuery(partialsQueryOptions(props.api));

    const action = useMutation({
        mutationFn: async (request: {
            partial: PartialActionRequest;
            resume: boolean;
        }): Promise<void> => {
            if (request.resume) await props.api.resumePartial(request.partial);
            else await props.api.discardPartial(request.partial);
        },
        onSuccess: async () => {
            await Promise.all([
                client.invalidateQueries({ queryKey: queryKeys.partials() }),
                client.invalidateQueries({ queryKey: queryKeys.transfers() }),
            ]);
        },
    });

    return (
        <section
            aria-label="Resumable transfers"
            className="mb-6 rounded-lg border border-slate-800 p-4"
        >
            <h2 className="mb-3 text-lg font-semibold">Resumable transfers</h2>
            {inventory.error ? (
                <p role="alert">{inventory.error.message}</p>
            ) : null}
            {action.error ? <p role="alert">{action.error.message}</p> : null}
            {inventory.data?.partials.length === 0 ? (
                <p className="text-sm text-slate-400">No resumable transfers</p>
            ) : null}
            {inventory.data?.partials.map((partial) => (
                <div
                    key={`${partial.dest_agent_id}:${partial.key}`}
                    className="flex flex-wrap items-center gap-3 border-t border-slate-800 py-3"
                >
                    <div className="min-w-0 flex-1 text-sm">
                        <p>
                            {partial.operation}: {partial.source.agent}:
                            {partial.source.path} → {partial.dest.agent}:
                            {partial.dest.path}
                        </p>
                        <p className="text-slate-400">
                            {formatSize(partial.partial_bytes)} /{" "}
                            {formatSize(partial.total_bytes)}
                        </p>
                    </div>
                    <Tooltip content="Verify the saved prefix and continue transferring">
                        <Button
                            disabled={action.isPending}
                            onClick={() =>
                                action.mutate({ partial, resume: true })
                            }
                        >
                            Resume
                        </Button>
                    </Tooltip>
                    <Tooltip content="Remove this partial file">
                        <Button
                            variant="secondary"
                            disabled={action.isPending}
                            onClick={() =>
                                action.mutate({ partial, resume: false })
                            }
                        >
                            Discard
                        </Button>
                    </Tooltip>
                </div>
            ))}
        </section>
    );
}
