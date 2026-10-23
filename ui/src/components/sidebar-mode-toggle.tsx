import { Columns2, PanelLeftClose, PanelsLeftRight } from "lucide-react";
import { useAtom } from "jotai";

import {
    SegmentedControl,
    type SegmentedControlOption,
} from "#ui/components/segmented-control";
import { sidebarModeAtom, type SidebarMode } from "#ui/side-menu-state";

const sidebarModeOptions = [
    {
        value: "auto",
        label: "Auto",
        icon: Columns2,
        tooltip: "Show the sidebars when the window is wide enough",
    },
    {
        value: "show",
        label: "Show",
        icon: PanelsLeftRight,
        tooltip: "Always show the sidebars",
    },
    {
        value: "hide",
        label: "Hide",
        icon: PanelLeftClose,
        tooltip: "Always hide the sidebars behind menu buttons",
    },
] as const satisfies ReadonlyArray<SegmentedControlOption<SidebarMode>>;

/** Lets the user pick the persistent-sidebar preference directly from the application menu. */
export function SidebarModeToggle() {
    const [sidebarMode, setSidebarMode] = useAtom(sidebarModeAtom);

    return (
        <SegmentedControl
            label="Sidebar mode"
            options={sidebarModeOptions}
            value={sidebarMode}
            onChange={setSidebarMode}
            className="mb-1"
        />
    );
}
