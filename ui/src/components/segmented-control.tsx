import type { LucideIcon } from "lucide-react";
import { cn } from "#ui/utils/cn";

import { ToggleButton } from "#ui/components/toggle-button";

/** Describes one mutually exclusive choice so every segment renders the same way. */
export type SegmentedControlOption<Value extends string> = {
    value: Value;
    label: string;
    icon: LucideIcon;
    tooltip: string;
};

/**
 * Shows every choice of a small enum side by side so the user can see the
 * current value and pick any other one directly instead of cycling through them.
 */
export function SegmentedControl<Value extends string>(props: {
    label: string;
    options: ReadonlyArray<SegmentedControlOption<Value>>;
    value: Value;
    onChange: (value: Value) => void;
    className?: string;
    /**
     * Shows only the icons in a narrow content column, for a control that shares a crowded
     * bar there. The labels still name each segment to assistive technology and in the
     * tooltips. The query names the `main` container because this control is its own
     * container for the sidebar layout.
     */
    iconOnlyWhenNarrow?: boolean;
    /**
     * Shows only the icons at every width, for a control tucked into a header where its
     * labels would crowd out the content beside it. The labels still name each segment to
     * assistive technology, so the tooltips must say which choice each icon is.
     */
    iconOnly?: boolean;
}) {
    return (
        <div
            role="group"
            aria-label={props.label}
            className={cn(
                "flex w-full min-w-0 gap-0.5 rounded-md border border-slate-700 bg-slate-900/60 p-0.5",
                // The container query only decides when icons fit beside labels. Icon-only
                // controls skip it, since containment would size them as if they were empty.
                !props.iconOnly && "@container",
                props.iconOnly && "w-auto",
                props.className,
            )}
        >
            {props.options.map((option) => {
                const Icon = option.icon;

                return (
                    <ToggleButton
                        key={option.value}
                        variant="subtle"
                        size="sm"
                        label={option.label}
                        pressed={option.value === props.value}
                        tooltip={option.tooltip}
                        tooltipClassName="min-w-0 flex-1"
                        onClick={() => props.onChange(option.value)}
                        className={cn(
                            "w-full min-w-0 gap-1.5 rounded px-1.5 py-1.5 text-xs aria-pressed:bg-slate-700 aria-pressed:text-slate-100",
                            props.iconOnly && "py-1",
                        )}
                    >
                        {/* Icons are decorative, so they give way first when the sidebar is narrow. */}
                        <Icon
                            className={cn(
                                "hidden h-3.5 w-3.5 shrink-0 @[15rem]:block",
                                props.iconOnlyWhenNarrow &&
                                    "@max-xl/main:block",
                                props.iconOnly && "block",
                            )}
                            aria-hidden="true"
                        />
                        {props.iconOnly ? null : (
                            <span
                                className={cn(
                                    "truncate",
                                    props.iconOnlyWhenNarrow &&
                                        "@max-xl/main:hidden",
                                )}
                            >
                                {option.label}
                            </span>
                        )}
                    </ToggleButton>
                );
            })}
        </div>
    );
}
