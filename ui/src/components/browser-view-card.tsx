import React from "react";

/**
 * Keeps full-width browser views on one consistent panel surface. It clips rather than
 * hides overflow, because a hidden overflow makes the card a scroll container and sticky
 * content inside it, such as diff file headers, would stick to the card instead of the page.
 */
export function BrowserViewCard(props: { children: React.ReactNode }) {
    return (
        <article className="w-full min-w-0 overflow-clip bg-surface shadow-2xl shadow-black/20">
            {props.children}
        </article>
    );
}
