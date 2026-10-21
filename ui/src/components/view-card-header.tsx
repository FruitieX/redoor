import React from "react";

/**
 * The heading every browser view card opens with: a blue icon, a small uppercase eyebrow that
 * names the kind of view, and the page title. One component keeps the details, sync, git and
 * image views identical, so a spacing or heading fix lands everywhere at once.
 */
export function ViewCardHeader(props: {
    icon?: React.ReactNode;
    /** Names the kind of view above the title, such as "Sync file". */
    eyebrow: string;
    title: string;
    /** Gives the heading a stable accessible name when the visible title is just a path. */
    titleAriaLabel?: string;
    /** Puts controls, such as path actions, at the trailing edge of the title row. */
    actions?: React.ReactNode;
    /** Adds supporting content below the title, such as a description or a link. */
    children?: React.ReactNode;
}) {
    return (
        <header className="overflow-hidden border-b border-slate-800 p-4 sm:p-5">
            <div className="flex min-w-0 items-start gap-2">
                {props.icon ? (
                    <div className="flex h-9 w-9 shrink-0 items-center justify-center text-blue-400">
                        {props.icon}
                    </div>
                ) : null}
                <div className="min-w-0 flex-1 pt-0.5">
                    <p className="mb-1 text-xs font-semibold uppercase tracking-[0.18em] text-blue-400">
                        {props.eyebrow}
                    </p>
                    <div className="flex flex-wrap items-center gap-3">
                        <h1
                            aria-label={props.titleAriaLabel}
                            className="break-all text-lg font-semibold text-slate-50"
                        >
                            {props.title}
                        </h1>
                        {props.actions ? (
                            <div className="ml-auto shrink-0">
                                {props.actions}
                            </div>
                        ) : null}
                    </div>
                </div>
            </div>
            {props.children}
        </header>
    );
}
