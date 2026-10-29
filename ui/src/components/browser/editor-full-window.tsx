import { getRouteApi } from "@tanstack/react-router";

const browserRouteApi = getRouteApi("/_app/agents/$deviceName/browser/$");

/**
 * Keeps the full-window editor in the URL, so Back leaves it, a reload keeps it, and a link
 * can open a file straight into it. Entering it adds a history entry for that reason, and the
 * browser route retains the parameter, so switching open files stays in it while each keyed
 * editor starts its own buffer. The normal layout is the default and stays out of the URL.
 */
export function useEditorFullWindow() {
    const navigate = browserRouteApi.useNavigate();
    const search = browserRouteApi.useSearch();

    return {
        isFullWindow: search.fullWindow === true,
        toggle: () =>
            navigate({
                search: (previous) => ({
                    ...previous,
                    fullWindow: previous.fullWindow === true ? undefined : true,
                }),
            }),
    };
}
