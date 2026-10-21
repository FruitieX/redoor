import { Search } from "lucide-react";

import { IconButton } from "#ui/components/icon-button";
import { useAgentSearchNavigation } from "#ui/search-navigation";

/** Opens the shared URL-owned search workflow from any agent-specific toolbar. */
export function SearchButton(props: { className?: string }) {
    const { openSearch } = useAgentSearchNavigation();

    return (
        <IconButton
            type="button"
            label="Search device"
            tooltip="Search device (s, Cmd/Ctrl+K)"
            onClick={() => void openSearch()}
            className={props.className}
        >
            <Search className="h-5 w-5" aria-hidden="true" />
        </IconButton>
    );
}
