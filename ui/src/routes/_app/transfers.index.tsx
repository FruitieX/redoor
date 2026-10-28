import { createFileRoute } from "@tanstack/react-router";
import { ArrowLeftRight } from "lucide-react";
import { useSuspenseQuery } from "@tanstack/react-query";

import { TransferList } from "#ui/components/transfer-list";
import { ResumableTransfers } from "#ui/components/resumable-transfers";
import { transfersQueryOptions } from "#ui/queries";
import { useAgents } from "#ui/use-agents";

export const Route = createFileRoute("/_app/transfers/")({
    component: TransfersPage,
});

function TransfersPage() {
    const { api } = Route.useRouteContext();
    const agents = useAgents();

    const { data: transferProgress } = useSuspenseQuery(
        transfersQueryOptions(api),
    );

    return (
        <div className="p-4 @xl:p-8">
            <div className="mx-auto max-w-7xl">
                <div className="mb-6 flex items-center gap-3">
                    <ArrowLeftRight className="h-6 w-6 shrink-0 text-blue-400" />
                    <h1 className="text-2xl font-bold text-slate-100">
                        Transfer history
                    </h1>
                </div>
                <ResumableTransfers api={api} />
                <div className="overflow-hidden rounded-lg border border-slate-800">
                    <TransferList
                        api={api}
                        agents={agents}
                        transfers={transferProgress.transfers}
                    />
                </div>
            </div>
        </div>
    );
}
