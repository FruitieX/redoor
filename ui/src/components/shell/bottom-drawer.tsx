import * as React from "react";
import { useAtom, useAtomValue } from "jotai";
import { ArrowUpDown, Files, SquareTerminal } from "lucide-react";

import { TabbedBottomDrawer } from "#ui/components/collapsible-bottom-panel";
import {
    SelectedFilesBadge,
    SelectedFilesPane,
} from "#ui/components/shell/selected-files-pane";
import {
    TransferProgressPane,
    TransferTabBadge,
} from "#ui/components/shell/transfers-pane";
import { TerminalPanel } from "#ui/components/terminal-panel";
import {
    bottomDrawerActivationAtom,
    bottomDrawerActiveTabAtom,
    bottomDrawerCollapsedAtom,
} from "#ui/bottom-drawer-state";
import { useTargetAgent } from "#ui/use-active-agent";
import { useAgents } from "#ui/use-agents";
import { useBrowserLocation } from "#ui/use-browser-location";
import { getParentPath } from "#ui/utils/path";

/**
 * Hosts the shared tools in one drawer. The badges and panes read their own data, so the
 * drawer only rerenders for its own state and never because transfers or selections move.
 */
export function ApplicationBottomDrawer(props: {
    isTerminalFullWindow: boolean;
    onTerminalFullWindowChange: (isFullWindow: boolean) => void;
}) {
    const drawerActivation = useAtomValue(bottomDrawerActivationAtom);
    const [activeTab, setActiveTab] = useAtom(bottomDrawerActiveTabAtom);
    const [isCollapsed, setIsCollapsed] = useAtom(bottomDrawerCollapsedAtom);
    const lastActivationRef = React.useRef(0);

    React.useEffect(() => {
        if (drawerActivation.sequence <= lastActivationRef.current) {
            return;
        }

        lastActivationRef.current = drawerActivation.sequence;
        setActiveTab(drawerActivation.tab);
        setIsCollapsed(false);
    }, [drawerActivation]);

    return (
        <TabbedBottomDrawer
            activeTab={activeTab}
            isCollapsed={isCollapsed}
            isFullWindow={props.isTerminalFullWindow}
            onActiveTabChange={setActiveTab}
            onCollapsedChange={setIsCollapsed}
            tabs={[
                {
                    id: "selected",
                    label: "Selected",
                    icon: <Files className="h-4 w-4" />,
                    badge: <SelectedFilesBadge />,
                    content: <SelectedFilesPane />,
                },
                {
                    id: "transfers",
                    label: "Transfers",
                    icon: <ArrowUpDown className="h-4 w-4" />,
                    badge: <TransferTabBadge />,
                    content: <TransferProgressPane />,
                },
                {
                    id: "terminal",
                    label: "Terminal",
                    icon: <SquareTerminal className="h-4 w-4" />,
                    content: (
                        <ShellTerminalPane
                            isVisible={!isCollapsed && activeTab === "terminal"}
                            isFullWindow={props.isTerminalFullWindow}
                            onFullWindowChange={
                                props.onTerminalFullWindowChange
                            }
                        />
                    ),
                },
            ]}
        />
    );
}

/**
 * Points the terminal at the agent the user is heading to, opening in the browsed directory
 * when there is one and in the agent's home otherwise. Missing paths still expose a directory so the
 * terminal opens near the attempted location.
 */
function ShellTerminalPane(props: {
    isVisible: boolean;
    isFullWindow: boolean;
    onFullWindowChange: (isFullWindow: boolean) => void;
}) {
    const agents = useAgents();
    const activeAgent = useTargetAgent();
    const browser = useBrowserLocation();

    // While navigating to another agent the browser location still belongs to the previous
    // one, whose directory means nothing on the new device.
    const browsedDirectory =
        browser === null || browser.agentId !== activeAgent?.id
            ? null
            : browser.entry === "file"
              ? getParentPath(browser.path)
              : browser.path;

    const activeTarget =
        activeAgent?.status === "connected" && activeAgent.cwd !== null
            ? { agent: activeAgent, cwd: browsedDirectory ?? activeAgent.cwd }
            : null;

    return (
        <TerminalPanel
            agents={agents}
            activeTarget={activeTarget}
            isVisible={props.isVisible}
            isFullWindow={props.isFullWindow}
            onFullWindowChange={props.onFullWindowChange}
        />
    );
}
