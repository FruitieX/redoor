import * as React from "react";

/**
 * Groups related detail values into one titled card, so every device page section shares the
 * same frame and heading style. The card is a labelled region, which gives assistive technology
 * and tests a stable way to find a section by its title.
 *
 * `actions` sits at the end of the header for status badges, and `bodyClassName` replaces the
 * default stacked layout when the content needs its own arrangement.
 */
export function DetailCard(props: {
    title: string;
    icon: React.ReactNode;
    children: React.ReactNode;
    actions?: React.ReactNode;
    bodyClassName?: string;
}) {
    const headingId = React.useId();

    return (
        <section
            aria-labelledby={headingId}
            className="rounded-lg border border-slate-800 bg-surface p-4"
        >
            <div className="mb-3 flex flex-wrap items-center justify-between gap-3">
                <div className="flex items-center gap-2 font-semibold text-slate-300">
                    {props.icon}
                    <h3
                        id={headingId}
                        className="text-sm uppercase tracking-wide"
                    >
                        {props.title}
                    </h3>
                </div>
                {props.actions}
            </div>
            <div className={props.bodyClassName ?? "space-y-2"}>
                {props.children}
            </div>
        </section>
    );
}
