import * as React from "react";

type TailwindBreakpoint = "sm" | "md" | "lg" | "xl" | "2xl";

/**
 * Container sizes the JS hook may compare against. Tailwind only emits the theme variables
 * that styles use, so a size belongs here only while `@xl:`-style classes reference it.
 */
type TailwindContainerSize = "xl";

/** Reads a Tailwind theme breakpoint so JS media queries stay aligned with `sm:` classes. */
function belowBreakpointQuery(breakpoint: TailwindBreakpoint): string {
    const value = getComputedStyle(document.documentElement)
        .getPropertyValue(`--breakpoint-${breakpoint}`)
        .trim();

    return `(width < ${value})`;
}

/**
 * Tracks the viewport against a Tailwind breakpoint instead of a hardcoded pixel width. Only
 * shell decisions such as drawers and modal dialogs belong here; page content must follow
 * the content column with `useIsContentBelow`, since the side menus can make it narrow.
 */
export function useIsBelowBreakpoint(breakpoint: TailwindBreakpoint): boolean {
    const [isBelow, setIsBelow] = React.useState(
        () => window.matchMedia(belowBreakpointQuery(breakpoint)).matches,
    );

    React.useEffect(() => {
        const media = window.matchMedia(belowBreakpointQuery(breakpoint));

        const update = () => {
            setIsBelow(media.matches);
        };

        update();
        media.addEventListener("change", update);

        return () => media.removeEventListener("change", update);
    }, [breakpoint]);

    return isBelow;
}

/**
 * The `@container/main` element around the routed content and its top and bottom chrome.
 * JS layout decisions measure it so they switch together with the `@xl:` style classes.
 */
export const ContentContainerContext = React.createContext<HTMLElement | null>(
    null,
);

/**
 * Converts a Tailwind container size token to pixels. The theme stores rems, and the root
 * font size follows the reader's browser setting, so the conversion cannot be a constant.
 */
function containerSizeInPixels(size: TailwindContainerSize): number {
    const root = getComputedStyle(document.documentElement);
    const value = root.getPropertyValue(`--container-${size}`).trim();

    return value.endsWith("rem")
        ? parseFloat(value) * parseFloat(root.fontSize)
        : parseFloat(value);
}

/**
 * Tracks the content column against a Tailwind container size, matching the `@max-xl:`
 * style variants. Media queries cannot see the column, which shrinks when side menus are
 * shown on a small screen.
 */
export function useIsContentBelow(size: TailwindContainerSize): boolean {
    const container = React.useContext(ContentContainerContext);
    const [isBelow, setIsBelow] = React.useState(false);

    // A layout effect settles the first measurement before paint, so wide controls never
    // flash in a narrow column.
    React.useLayoutEffect(() => {
        if (!container) {
            return;
        }

        const threshold = containerSizeInPixels(size);

        const update = () => {
            setIsBelow(container.getBoundingClientRect().width < threshold);
        };

        update();
        const observer = new ResizeObserver(update);
        observer.observe(container);

        return () => observer.disconnect();
    }, [container, size]);

    return isBelow;
}
