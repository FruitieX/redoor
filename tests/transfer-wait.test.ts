import { QueryClient } from "@tanstack/react-query";
import { expect, test } from "vitest";

import type { TransferProgressEntry } from "#bindings/TransferProgressEntry";
import { ApiClient } from "#ui/api-client";
import { queryKeys } from "#ui/queries";
import { waitForTransfers } from "#ui/transfer-wait";

/** Builds a complete transfer row so tests only spell out the fields they vary. */
function transfer(
    requestId: number,
    overrides: Partial<TransferProgressEntry> = {},
): TransferProgressEntry {
    return {
        request_id: requestId,
        agent_id: "agent",
        path: "/copy",
        source: null,
        dest: null,
        direction: "copy",
        total_bytes: 10,
        transferred_bytes: 10,
        started_at: 1,
        ended_at: null,
        state: "completed",
        cancelable: false,
        error: null,
        atomic: false,
        pausable: false,
        verifying: false,
        resumed_bytes: 0,
        ...overrides,
    };
}

/** Serves the given rows from the public transfers endpoint without a server. */
function apiWithTransfers(rows: TransferProgressEntry[]): ApiClient {
    const api = new ApiClient("http://localhost");
    api.getTransferProgress = async () => ({ transfers: rows });

    return api;
}

test("resolves true once the awaited transfer completes", async () => {
    const queryClient = new QueryClient();

    const settled = await waitForTransfers({
        queryClient,
        api: apiWithTransfers([transfer(7)]),
        isSettled: (transfers) =>
            transfers.some(
                (entry) =>
                    entry.request_id === 7 && entry.state === "completed",
            ),
    });

    // The wait must report success only after the shared query delivered the finished row.
    expect(settled).toBe(true);
});

test("ignores transfers cached before the wait began", async () => {
    const queryClient = new QueryClient();
    const api = apiWithTransfers([transfer(8)]);

    // An older list in the shared cache cannot mention a transfer that did not exist yet.
    queryClient.setQueryData(
        queryKeys.transfers(),
        { transfers: [transfer(1)] },
        { updatedAt: 1 },
    );

    const seen: number[][] = [];

    await waitForTransfers({
        queryClient,
        api,
        isSettled: (transfers) => {
            seen.push(transfers.map((entry) => entry.request_id));

            return true;
        },
    });

    // Only the list fetched for this wait reaches the check, so it never judges stale data.
    expect(seen).toEqual([[8]]);
});

test("rejects when the check throws, such as for an errored transfer", async () => {
    const queryClient = new QueryClient();

    const outcome = waitForTransfers({
        queryClient,
        api: apiWithTransfers([
            transfer(9, { state: "errored", error: "disk full" }),
        ]),
        isSettled: (transfers) => {
            const failed = transfers.find((entry) => entry.state === "errored");

            if (failed) {
                throw new Error(failed.error ?? "failed");
            }

            return false;
        },
    });

    // The caller's own failure message must surface instead of waiting forever.
    await expect(outcome).rejects.toThrow("disk full");
});

test("rejects when the transfers cannot be read", async () => {
    const queryClient = new QueryClient();
    const api = new ApiClient("http://localhost");

    api.getTransferProgress = async () => {
        throw new Error("server unreachable");
    };

    const outcome = waitForTransfers({
        queryClient,
        api,
        isSettled: () => false,
    });

    // A failed read must reach the caller at once, not be retried silently for a long time.
    await expect(outcome).rejects.toThrow("server unreachable");
});

test("resolves false when aborted before anything settles", async () => {
    const queryClient = new QueryClient();
    const controller = new AbortController();
    controller.abort();

    const settled = await waitForTransfers({
        queryClient,
        api: apiWithTransfers([]),
        isSettled: () => false,
        signal: controller.signal,
    });

    // An aborted wait must tell its caller not to update state that may be gone.
    expect(settled).toBe(false);
});
