// @vitest-environment jsdom

import { cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { expect, onTestFinished, test, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
    createMemoryHistory,
    createRootRoute,
    createRouter,
    RouterContextProvider,
} from "@tanstack/react-router";
import { Button } from "#ui/components/button";

import {
    useBrowserRefreshTriggers,
    useEditorRefreshRegistration,
} from "./refresh";

/** Exercises focus events through the same refresh hooks used by the browser. */
function RefreshHarness() {
    useEditorRefreshRegistration({
        agentId: "agent-1",
        path: "/workspace/file.txt",
    });
    useBrowserRefreshTriggers("agent-1");

    return (
        <>
            <div data-terminal-input>
                <textarea aria-label="Terminal input" />
                <Button type="button">Terminal control</Button>
            </div>
            <Button type="button">Editor</Button>
        </>
    );
}

/** Real providers keep query and router behavior in the test while spies observe refresh coordination. */
async function renderRefreshHarness() {
    const queryClient = new QueryClient();

    const router = createRouter({
        routeTree: createRootRoute(),
        history: createMemoryHistory({ initialEntries: ["/"] }),
    });

    onTestFinished(() => {
        cleanup();
        queryClient.clear();
        vi.restoreAllMocks();
    });
    await router.load();

    const mocks = {
        invalidateQueries: vi.spyOn(queryClient, "invalidateQueries"),
        refetchQueries: vi.spyOn(queryClient, "refetchQueries"),
        invalidateRoute: vi.spyOn(router, "invalidate"),
    };

    const view = render(
        <QueryClientProvider client={queryClient}>
            <RouterContextProvider router={router}>
                <RefreshHarness />
            </RouterContextProvider>
        </QueryClientProvider>,
    );

    return { view, mocks };
}

test("refreshes the listing and the editor's device text when terminal focus leaves", async () => {
    const { view, mocks } = await renderRefreshHarness();

    const terminalInput = view.getByRole("textbox", {
        name: "Terminal input",
    });

    const editor = view.getByRole("button", { name: "Editor" });

    fireEvent.focusOut(terminalInput, { relatedTarget: editor });

    // Route invalidation must follow the query refresh triggered by terminal blur.
    await waitFor(() => expect(mocks.invalidateRoute).toHaveBeenCalledOnce());
    // Creating a file can navigate while this git refresh is already fetching.
    expect(mocks.invalidateQueries).toHaveBeenCalledWith(
        {
            queryKey: ["server-state", "agent", "agent-1", "git"],
        },
        {
            cancelRefetch: false,
        },
    );
    // The editor pulls new device bytes before route invalidation reloads the listing; even a
    // dirty editor needs them to warn that the device changed, and never lets them replace edits.
    expect(mocks.refetchQueries).toHaveBeenCalledWith(
        {
            queryKey: [
                "server-state",
                "agent",
                "agent-1",
                "filesystem",
                "content",
                "/workspace/file.txt",
            ],
            type: "all",
        },
        {
            // Concurrent CodeMirror focus must join this fetch instead of canceling it.
            cancelRefetch: false,
        },
    );
});

test("ignores focus movement within the terminal", async () => {
    const { view, mocks } = await renderRefreshHarness();

    const terminalInput = view.getByRole("textbox", {
        name: "Terminal input",
    });

    const terminalControl = view.getByRole("button", {
        name: "Terminal control",
    });

    fireEvent.focusOut(terminalInput, { relatedTarget: terminalControl });

    // Internal terminal focus changes must not cause browser network traffic.
    expect(mocks.invalidateRoute).not.toHaveBeenCalled();
    expect(mocks.refetchQueries).not.toHaveBeenCalled();
});
