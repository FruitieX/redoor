import * as React from "react";
import { useRouterState } from "@tanstack/react-router";
import { atom, useSetAtom } from "jotai";

import { useBrowserLocation } from "#ui/use-browser-location";
import { atomWithLocalStorage } from "./utils/local-storage-atom";

export type AgentTabLocations = Record<string, string>;

/** Keeps every agent tab's last route available across switches and reloads. */
export const agentTabLocationsAtom = atomWithLocalStorage<AgentTabLocations>(
    "redoor.agent-tab-locations",
    {},
);

/** Records one route without replacing remembered locations for other agents. */
export const rememberAgentTabLocationAtom = atom(
    null,
    (
        get,
        set,
        location: {
            agentId: string;
            pathname: string;
        },
    ) => {
        const locations = get(agentTabLocationsAtom);

        if (locations[location.agentId] === location.pathname) {
            return;
        }

        set(agentTabLocationsAtom, {
            ...locations,
            [location.agentId]: location.pathname,
        });
    },
);

/**
 * Uses a remembered route only when it still belongs to the requested agent,
 * preventing malformed localStorage data from making one tab open another.
 */
export function getAgentTabLocation(
    locations: AgentTabLocations,
    agentId: string,
    fallback: string,
): string {
    const rememberedLocation = locations[agentId];
    const agentPrefix = `/agents/${encodeURIComponent(agentId)}`;

    if (
        rememberedLocation === agentPrefix ||
        rememberedLocation?.startsWith(`${agentPrefix}/`)
    ) {
        return rememberedLocation;
    }

    return fallback;
}

/**
 * Remembers the file-browser location of the connected agent on screen, so switching away
 * from a device and back returns to where the user was. Only browser routes of a connected
 * agent qualify, because other pages are cheap to reach again.
 */
export function useRememberAgentTabLocation() {
    const rememberAgentTabLocation = useSetAtom(rememberAgentTabLocationAtom);
    const browser = useBrowserLocation();

    // The resolved location changes together with the matched routes. The live location moves
    // first, which would pair the next page's path with the previous page's agent.
    const pathname = useRouterState({
        select: (state) => state.resolvedLocation?.pathname,
    });

    // The browser location comes from the same committed matches as the pathname, so the two
    // always describe the same page. A browser route that loaded for a disconnected agent
    // shows the lifecycle screen instead and is not worth returning to.
    const agentId = browser?.isConnected ? browser.agentId : null;

    React.useEffect(() => {
        if (agentId === null || pathname === undefined) {
            return;
        }

        rememberAgentTabLocation({ agentId, pathname });
    }, [agentId, pathname, rememberAgentTabLocation]);
}
