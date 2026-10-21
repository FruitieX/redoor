import { Maximize2, Minimize2 } from "lucide-react";
import { IconButton } from "#ui/components/icon-button";

/** Keeps full-window controls consistent while naming the surface they resize. */
export function FullWindowToggle(props: {
    targetName: string;
    isFullWindow: boolean;
    onToggle: () => void;
}) {
    return (
        <IconButton
            type="button"
            label={
                props.isFullWindow
                    ? `Restore ${props.targetName} size`
                    : `Expand ${props.targetName} to full window`
            }
            aria-pressed={props.isFullWindow}
            onClick={props.onToggle}
            variant="ghost"
            size="lg"
            className="shrink-0"
        >
            {props.isFullWindow ? (
                <Minimize2 className="h-4 w-4" aria-hidden="true" />
            ) : (
                <Maximize2 className="h-4 w-4" aria-hidden="true" />
            )}
        </IconButton>
    );
}
