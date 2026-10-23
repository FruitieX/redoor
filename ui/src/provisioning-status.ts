import * as React from "react";
import { atom, useSetAtom } from "jotai";
import { z } from "zod";

import type { ProvisioningStatusMessage } from "#bindings/ProvisioningStatusMessage";
import type { Agent } from "#ui/api-client";

const STORAGE_KEY = "redoor.provisioning-status";

/** Rejects leftover or hand-edited storage so a bad payload cannot break the home card. */
export const provisioningStatusMessageSchema = z.object({
    message: z.string(),
    at: z.number(),
});

const provisioningStatusStoreSchema = z.record(
    z.string(),
    z.array(provisioningStatusMessageSchema),
);

export type ProvisioningStatusStore = z.infer<
    typeof provisioningStatusStoreSchema
>;

/** Parses the localStorage JSON string at the I/O boundary. */
export function parseProvisioningStatusStore(
    raw: string,
): ProvisioningStatusStore {
    try {
        return provisioningStatusStoreSchema.parse(JSON.parse(raw));
    } catch {
        return {};
    }
}

/** Reads the name-keyed history only after Zod accepts the stored JSON. */
export function readProvisioningStatusStore(): ProvisioningStatusStore {
    try {
        const raw = globalThis.localStorage.getItem(STORAGE_KEY);

        if (raw === null) {
            return {};
        }

        return parseProvisioningStatusStore(raw);
    } catch {
        return {};
    }
}

/** Writes the already-validated map so later reads do not see partial junk. */
function writeProvisioningStatusStore(store: ProvisioningStatusStore) {
    try {
        globalThis.localStorage.setItem(STORAGE_KEY, JSON.stringify(store));
    } catch {
        // Persistence is best-effort; the current session still has the jotai copy.
    }
}

/** Survives the starting-page unmount so the agent home can show the last attempt. */
export const provisioningStatusStoreAtom = atom(readProvisioningStatusStore());

/** Remembers one agent's non-empty list without erasing other agents. */
export const rememberProvisioningStatusAtom = atom(
    null,
    (
        get,
        set,
        update: {
            agentName: string;
            messages: ProvisioningStatusMessage[];
        },
    ) => {
        if (update.messages.length === 0) {
            return;
        }

        const next = {
            ...get(provisioningStatusStoreAtom),
            [update.agentName]: update.messages,
        };

        set(provisioningStatusStoreAtom, next);
        writeProvisioningStatusStore(next);
    },
);

/**
 * Persists an agent's provisioning messages while any of its pages is open. The messages only
 * exist in the inventory during a start attempt, so they must be saved as they arrive for the
 * home page to show the last attempt after the agent connected. Writing to localStorage is
 * the external side effect that justifies the effect.
 */
export function useRememberProvisioningStatus(agent: Agent | undefined) {
    const rememberProvisioningStatus = useSetAtom(
        rememberProvisioningStatusAtom,
    );

    const agentName = agent?.name;
    const provisioningStatus = agent?.provisioningStatus;

    React.useEffect(() => {
        if (agentName === undefined || !provisioningStatus?.length) {
            return;
        }

        rememberProvisioningStatus({
            agentName,
            messages: provisioningStatus,
        });
    }, [agentName, provisioningStatus, rememberProvisioningStatus]);
}
