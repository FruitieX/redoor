import * as React from "react";
import { atom, useSetAtom } from "jotai";

import { useBrowserLocation } from "#ui/use-browser-location";
import { atomWithLocalStorage } from "./utils/local-storage-atom";

/** Maps a device id to the filesystem path last browsed on it. */
export type AgentTabLocations = Record<string, string>;

/**
 * Keeps every device's last browsed path available across switches and reloads. Entries are
 * keyed by device id and hold a filesystem path rather than a URL, because URLs carry the
 * device name, which a rename would leave pointing at nothing or at another device.
 */
export const agentTabLocationsAtom = atomWithLocalStorage<AgentTabLocations>(
    "redoor.device-browser-paths",
    {},
);

/** Records one path without replacing remembered locations for other devices. */
export const rememberAgentTabLocationAtom = atom(
    null,
    (
        get,
        set,
        location: {
            agentId: string;
            path: string;
        },
    ) => {
        const locations = get(agentTabLocationsAtom);

        if (locations[location.agentId] === location.path) {
            return;
        }

        set(agentTabLocationsAtom, {
            ...locations,
            [location.agentId]: location.path,
        });
    },
);

/**
 * Uses a remembered path only when it is a real absolute path, preventing malformed
 * localStorage data from producing a broken browser route.
 */
export function getAgentTabLocation(
    locations: AgentTabLocations,
    agentId: string,
    fallback: string,
): string {
    const rememberedPath = locations[agentId];

    return rememberedPath?.startsWith("/") ? rememberedPath : fallback;
}

/**
 * Remembers the file-browser location of the connected agent on screen, so switching away
 * from a device and back returns to where the user was. Only browser routes of a connected
 * agent qualify, because other pages are cheap to reach again.
 */
export function useRememberAgentTabLocation() {
    const rememberAgentTabLocation = useSetAtom(rememberAgentTabLocationAtom);
    const browser = useBrowserLocation();

    // The browser location comes from the committed matches, so its path and device always
    // describe the same page. A browser route that loaded for a disconnected agent shows the
    // lifecycle screen instead and is not worth returning to.
    const agentId = browser?.isConnected ? browser.agentId : null;
    const path = browser?.path;

    React.useEffect(() => {
        if (agentId === null || path === undefined) {
            return;
        }

        rememberAgentTabLocation({ agentId, path });
    }, [agentId, path, rememberAgentTabLocation]);
}
