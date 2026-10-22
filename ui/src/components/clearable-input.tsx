import * as React from "react";
import { X } from "lucide-react";
import { twMerge } from "tailwind-merge";

import { IconButton } from "#ui/components/icon-button";
import { InputControl } from "#ui/components/input-control";

/**
 * Gives filter-style inputs a one-click reset, because clearing a short query by selecting
 * and deleting it is slow on touch screens. The clear button only appears when there is
 * something to clear, and it returns focus to the input so typing can continue.
 */
export function ClearableInput(props: {
    /** Names the input for assistive technology since filter fields have no visible label. */
    label: string;
    /** Names the clear action; it defaults to a generic phrase when one is enough. */
    clearLabel?: string;
    value: string;
    placeholder: string;
    className?: string;
    onChange: (value: string) => void;
}) {
    const inputRef = React.useRef<HTMLInputElement>(null);

    return (
        <div className={twMerge("relative", props.className)}>
            <InputControl
                ref={inputRef}
                type="text"
                aria-label={props.label}
                value={props.value}
                placeholder={props.placeholder}
                autoComplete="off"
                spellCheck={false}
                onChange={(event) => props.onChange(event.target.value)}
                className="w-full py-1.5 pr-8 text-sm placeholder:text-slate-600"
            />
            {props.value !== "" ? (
                <IconButton
                    type="button"
                    label={props.clearLabel ?? "Clear"}
                    onClick={() => {
                        props.onChange("");
                        inputRef.current?.focus();
                    }}
                    variant="quiet"
                    size="xs"
                    // The tooltip wraps the button, so the wrapper is what must be positioned.
                    tooltipClassName="absolute top-1/2 right-1.5 -translate-y-1/2"
                >
                    <X className="h-3.5 w-3.5" aria-hidden="true" />
                </IconButton>
            ) : null}
        </div>
    );
}
