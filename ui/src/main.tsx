import { StrictMode } from "react";
import ReactDOM from "react-dom/client";
import { Provider, createStore } from "jotai";
import { RouterProvider, createRouter } from "@tanstack/react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

// Import the generated route tree
import { routeTree } from "./routeTree.gen";

import "./styles.css";
import { ApiClient } from "#ui/api-client";
import { RouteError } from "#ui/components/route-error";
import { ShortcutRegistry, ShortcutRegistryContext } from "#ui/shortcuts";
import { TerminalSessions } from "#ui/terminal/terminal-sessions";
import { findCachedAgent, invalidateAllAgentData } from "#ui/queries";
import { UploadQueue } from "#ui/upload-queue";

// The API lives on the same origin as the page: the redoor binary
// embeds the UI alongside the API on a single port, and during local
// `vite dev` the Vite proxy in `vite.config.ts` forwards `/api` and
// `/ws` to the redoor server. Either way, using the page's origin
// keeps the client configuration trivial.
export const api = new ApiClient(window.location.origin);

export const queryClient = new QueryClient();

// The store is created here, rather than by the Provider, so the terminal owner can read the
// drawer atoms and reveal the drawer without going through a component.
const store = createStore();

// Terminals are a page-lifetime resource: shells keep running while routes and the drawer
// come and go. Logging out or losing the session reloads the page, which ends them.
const terminals = new TerminalSessions(queryClient, store);

// Uploads outlive the page being browsed, so the queue lives with the application and only
// the views that enqueue into it or show it subscribe.
const uploads = new UploadQueue({
    queryClient,
    resolveAgent: (agentId) => findCachedAgent(queryClient, api, agentId),
    onSettled: () => {
        void invalidateAllAgentData(queryClient);
    },
    unloadTarget: window,
});

// One registry owns every window-level shortcut, so the order in which commands claim a key
// is decided by explicit priority instead of by whichever component registered first.
const shortcuts = new ShortcutRegistry(window);

// Create a new router instance
const router = createRouter({
    routeTree,
    context: {
        api,
        queryClient,
        terminals,
        uploads,
    },
    defaultPreload: "intent",
    scrollRestoration: true,
    defaultStructuralSharing: true,
    defaultPreloadStaleTime: 0,
    // Replaces TanStack's bare "Something went wrong!" panel with themed details.
    defaultErrorComponent: RouteError,
});

// Any 401 means the session ended, so reload into the login form with the current URL as the
// return path. The reload also drops cached server state that belongs to the old session.
api.setUnauthorizedHandler(() => {
    // The login form's own requests answer 401 for wrong credentials, which must not loop.
    if (router.matchRoute({ to: "/login" })) {
        return;
    }

    const returnPath = `${window.location.pathname}${window.location.search}`;

    window.location.replace(
        router.buildLocation({
            to: "/login",
            search: { redirect: returnPath },
        }).href,
    );
});

// Register the router instance for type safety
declare module "@tanstack/react-router" {
    interface Register {
        router: typeof router;
    }
}

// Render the app
const rootElement = document.getElementById("app");

if (rootElement && !rootElement.innerHTML) {
    const root = ReactDOM.createRoot(rootElement);
    root.render(
        <StrictMode>
            <Provider store={store}>
                <ShortcutRegistryContext value={shortcuts}>
                    <QueryClientProvider client={queryClient}>
                        <RouterProvider router={router} />
                    </QueryClientProvider>
                </ShortcutRegistryContext>
            </Provider>
        </StrictMode>,
    );
}
