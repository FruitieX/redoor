import { useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi, Link } from "@tanstack/react-router";

import type { TransferProgressEntry } from "#bindings/TransferProgressEntry";
import { TransferList } from "#ui/components/transfer-list";
import { transfersQueryOptions } from "#ui/queries";
import { useAgents } from "#ui/use-agents";
import { getLastEstimatedTransferProgress } from "#ui/utils/transfer-progress";
import { useTransferClock } from "#ui/utils/use-transfer-clock";

const rootRouteApi = getRouteApi("__root__");

/** Reads the shared transfer poll so each drawer leaf subscribes on its own. */
function useTransfers(): TransferProgressEntry[] {
    const { api } = rootRouteApi.useRouteContext();

    return useSuspenseQuery(transfersQueryOptions(api)).data.transfers;
}

/** Tells the badge clock whether any active transfer has an ETA worth animating. */
function hasEstimatedFinish(
    transfers: TransferProgressEntry[],
    refreshedAtMilliseconds: number,
): boolean {
    return (
        getLastEstimatedTransferProgress(
            transfers,
            0,
            refreshedAtMilliseconds,
        ) !== null
    );
}

/**
 * Animates the tab badge from the transfer whose ETA decides when all transfers finish.
 * It runs the animation-frame clock, so it must stay a leaf: anywhere higher it would
 * rerender the drawer, the terminal and every session on each frame.
 */
export function TransferTabBadge() {
    const transfers = useTransfers();
    const clock = useTransferClock(transfers, hasEstimatedFinish);

    const progress = getLastEstimatedTransferProgress(
        transfers,
        clock.elapsedSeconds,
        clock.refreshedAtMilliseconds,
    );

    return (
        <>
            {progress === null
                ? getTransferSummary(transfers) || "0"
                : `${Math.round(progress.percentage)}%`}
        </>
    );
}

/** Summarizes all transfer states without allowing polling updates to activate the drawer. */
function getTransferSummary(transfers: TransferProgressEntry[]) {
    const activeTransfers = transfers.filter(
        (transfer) => transfer.state === "active",
    );

    return [
        [
            activeTransfers.filter(
                (transfer) => transfer.direction === "download",
            ).length,
            "downloading",
        ],
        [
            activeTransfers.filter(
                (transfer) => transfer.direction === "upload",
            ).length,
            "uploading",
        ],
        [
            activeTransfers.filter((transfer) => transfer.direction === "edit")
                .length,
            "editing",
        ],
        [
            activeTransfers.filter((transfer) => transfer.direction === "copy")
                .length,
            "copying",
        ],
        [
            activeTransfers.filter((transfer) => transfer.direction === "move")
                .length,
            "moving",
        ],
        [
            transfers.filter((transfer) => transfer.state === "completed")
                .length,
            "completed",
        ],
        [
            transfers.filter((transfer) => transfer.state === "errored").length,
            "errored",
        ],
        [
            transfers.filter((transfer) => transfer.state === "canceled")
                .length,
            "canceled",
        ],
    ]
        .flatMap(([count, label]) => (count !== 0 ? [`${count} ${label}`] : []))
        .join(", ");
}

/** Shows active transfers inside the shared drawer while history remains on its route. */
export function TransferProgressPane() {
    const { api } = rootRouteApi.useRouteContext();
    const agents = useAgents();
    const transfers = useTransfers();

    const activeTransfers = transfers.filter(
        (transfer) =>
            transfer.state === "active" || transfer.state === "canceling",
    );

    return (
        <div className="min-h-0 flex-1 overflow-auto rounded-md border border-slate-800">
            {activeTransfers.length === 0 ? (
                <div className="p-6 text-center text-sm text-slate-500">
                    No active transfers.{" "}
                    <Link
                        to="/transfers"
                        className="text-blue-400 hover:underline"
                    >
                        View all
                    </Link>
                </div>
            ) : (
                <TransferList
                    api={api}
                    agents={agents}
                    transfers={activeTransfers}
                />
            )}
        </div>
    );
}
