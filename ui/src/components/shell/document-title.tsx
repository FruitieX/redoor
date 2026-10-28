import { useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi, useParams } from "@tanstack/react-router";
import { useEffect } from "react";

import type { ServerInfoResponse } from "#bindings/ServerInfoResponse";
import { serverInfoQueryOptions } from "#ui/queries";
import { getBaseName } from "#ui/utils/path";

const rootRouteApi = getRouteApi("__root__");

/**
 * Builds the browser tab title. The server hostname and version lead every title so tabs of
 * different servers, or of a server before and after an upgrade, can be told apart. In the
 * file browser the viewed entry and its agent come first, because that is what distinguishes
 * many open tabs of the same server.
 */
export function documentTitle(
    serverInfo: Pick<ServerInfoResponse, "hostname" | "version">,
    browser: { deviceName: string; path: string } | null,
): string {
    const serverTitle = `${serverInfo.hostname} | Redoor ${serverInfo.version}`;

    if (browser === null) {
        return serverTitle;
    }

    return `${getBaseName(browser.path)}@${browser.deviceName} | ${serverTitle}`;
}

/**
 * Keeps `document.title` in sync with the authenticated page. It reads the URL params rather
 * than the browser loader data, so the title follows navigation immediately instead of
 * waiting for the listing to load.
 */
export function DocumentTitle() {
    const { api } = rootRouteApi.useRouteContext();

    const serverInfo = useSuspenseQuery(serverInfoQueryOptions(api)).data;

    const browserParams = useParams({
        from: "/_app/agents/$deviceName/browser/$",
        shouldThrow: false,
    });

    const title = documentTitle(
        serverInfo,
        browserParams
            ? {
                  deviceName: browserParams.deviceName,
                  path: `/${browserParams._splat ?? ""}`,
              }
            : null,
    );

    useEffect(() => {
        const previousTitle = document.title;
        document.title = title;

        // Restores the static title when the shell unmounts, such as on logout to the login page.
        return () => {
            document.title = previousTitle;
        };
    }, [title]);

    return null;
}
