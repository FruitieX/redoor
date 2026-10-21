import * as React from "react";
import { Outlet, getRouteApi } from "@tanstack/react-router";

import { useRememberAgentTabLocation } from "#ui/agent-tab-locations";
import { AgentNavigation } from "#ui/components/agent-navigation";
import { ApplicationNavigation } from "#ui/components/application-navigation";
import { ContextualTopBar } from "#ui/components/contextual-top-bar";
import { GlobalFileImportHandler } from "#ui/components/global-file-import-handler";
import { OverlayChromeLayout } from "#ui/components/overlay-chrome-layout";
import { ApplicationBottomDrawer } from "#ui/components/shell/bottom-drawer";
import { RouteLoadingIndicator } from "#ui/components/shell/route-loading-indicator";
import { ThemeManager } from "#ui/components/theme-toggle";
import { UserStatePersistToast } from "#ui/components/user-state-persist-toast";
import { invalidateAllAgentData } from "#ui/queries";
import { useRefreshListener } from "#ui/refresh-listener";
import { UploadQueueManager } from "#ui/upload-queue";
import { useAgents } from "#ui/use-agents";
import { useBrowserLocation } from "#ui/use-browser-location";
import { useEscapeBlursTextControls } from "#ui/utils/use-escape-blur";

const rootRouteApi = getRouteApi("__root__");

/**
 * Frames every authenticated page with the menus, the top bar and the shared tools drawer.
 * It only exists below the `_app` layout route, so everything that needs a valid session,
 * from the server event connection to the upload scheduler, starts and stops with it.
 */
export function AppShell() {
    const { api } = rootRouteApi.useRouteContext();

    const [isTerminalFullWindow, setIsTerminalFullWindow] =
        React.useState(false);

    useRefreshListener(api);
    useRememberAgentTabLocation();
    useEscapeBlursTextControls();

    return (
        <div className="flex h-dvh overflow-hidden bg-[#0b0d12]">
            <ThemeManager />
            <RouteLoadingIndicator />
            <UserStatePersistToast />
            <ShellFileImportHandler />
            <ShellUploadQueue />
            <ApplicationNavigation />
            <OverlayChromeLayout
                isBottomChromeFullWindow={isTerminalFullWindow}
                topChrome={<ContextualTopBar />}
                bottomChrome={
                    <ApplicationBottomDrawer
                        isTerminalFullWindow={isTerminalFullWindow}
                        onTerminalFullWindowChange={setIsTerminalFullWindow}
                    />
                }
            >
                <Outlet />
            </OverlayChromeLayout>
            <AgentNavigation />
        </div>
    );
}

/**
 * Accepts dropped and pasted files into the directory on screen, but only while its agent
 * is connected and able to write there.
 */
function ShellFileImportHandler() {
    const agents = useAgents();
    const browser = useBrowserLocation();

    const agent =
        browser?.entry === "directory"
            ? agents.find((entry) => entry.id === browser.agentId)
            : undefined;

    const destination =
        browser && agent?.status === "connected" && agent.cwd !== null
            ? { agent, path: browser.path }
            : null;

    return <GlobalFileImportHandler destination={destination} />;
}

/** Runs the upload scheduler for the whole session and refreshes listings its uploads change. */
function ShellUploadQueue() {
    const { queryClient } = rootRouteApi.useRouteContext();
    const agents = useAgents();

    return (
        <UploadQueueManager
            agents={agents}
            onUploadsChanged={() => invalidateAllAgentData(queryClient)}
        />
    );
}
