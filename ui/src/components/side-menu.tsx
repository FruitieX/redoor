import * as React from "react";
import { X } from "lucide-react";
import { useAtom } from "jotai";
import { cn } from "#ui/utils/cn";
import { IconButton } from "#ui/components/icon-button";
import { shortcutPriority, useShortcut } from "#ui/shortcuts";
import {
    openSideMenuAtom,
    sideMenuDrawerIds,
    sidebarWidthsAtom,
    usePersistentSideMenus,
    type SideMenuId,
} from "#ui/side-menu-state";
import { shouldIgnoreKeyboardShortcut } from "#ui/utils/keyboard";
import { focusableSelector, useFocusTrap } from "#ui/utils/use-focus-trap";

/**
 * Provides the shared wide-screen sidebar and responsive drawer behavior for either edge.
 * The menu reads and closes its own open state from the shared atom, so the shell and the
 * controls that open it never have to pass state or focus targets around.
 */
export function SideMenu(props: {
    menu: SideMenuId;
    placement: "left" | "right";
    label: string;
    /**
     * Drops the panel padding for content that draws its own edge-to-edge chrome, such as
     * an icon rail, which would otherwise float inside a margin.
     */
    flush?: boolean;
    children: React.ReactNode;
}) {
    const drawerRef = React.useRef<HTMLElement>(null);
    const resize = useSideMenuResize(props.placement);
    const panelId = React.useId();
    const [openMenu, setOpenMenu] = useAtom(openSideMenuAtom);
    const isPersistent = usePersistentSideMenus();
    const drawerId = sideMenuDrawerIds[props.menu];
    const isOpen = openMenu === props.menu;

    const onClose = React.useCallback(() => setOpenMenu(null), [setOpenMenu]);

    React.useEffect(() => {
        // A persistent sidebar replaces the drawer, so a request left over from a narrower
        // window must not pop the drawer open again when the window shrinks.
        if (isPersistent && isOpen) {
            onClose();
        }
    }, [isPersistent, isOpen, onClose]);

    React.useEffect(() => {
        if (!isOpen) {
            return;
        }

        // Return focus to the control that declares it opens this drawer. Looking it up, rather
        // than remembering document.activeElement, keeps the target right when the drawer was
        // opened programmatically or by a browser that does not focus clicked buttons, and
        // when another drawer was closing and had just moved focus.
        const opener = document.querySelector<HTMLElement>(
            `[aria-controls="${drawerId}"]`,
        );

        const drawer = drawerRef.current;

        const focusable =
            drawer?.querySelectorAll<HTMLElement>(focusableSelector);

        focusable?.item(0).focus();

        return () => {
            opener?.focus();
        };
    }, [isOpen, drawerId]);

    // The open drawer is the topmost layer, so dismissing it wins over page shortcuts. A
    // keyboard sort inside it claims Escape first, at a higher tier in the capture phase.
    useShortcut(
        {
            name: "side-menu-dismiss",
            phase: "bubble",
            priority: shortcutPriority.modal,
            match: (event) => event.key === "Escape",
            run: onClose,
        },
        isOpen,
    );

    useFocusTrap(drawerRef, isOpen);

    const borderClass = props.placement === "left" ? "border-r" : "border-l";
    const edgeClass = props.placement === "left" ? "left-0" : "right-0";

    return (
        <>
            <aside
                id={panelId}
                aria-label={props.label}
                style={{ width: resize.width }}
                className={cn(
                    isPersistent ? "flex" : "hidden",
                    "relative min-w-0 shrink-0 flex-col border-slate-800 bg-chrome",
                    borderClass,
                    !props.flush && "p-3",
                )}
            >
                {props.children}
                {isPersistent ? (
                    <div
                        role="separator"
                        aria-label={`Resize ${props.label.toLowerCase()}`}
                        aria-controls={panelId}
                        aria-orientation="vertical"
                        aria-description="Drag or use the left and right arrow keys to resize"
                        aria-valuemin={180}
                        aria-valuemax={resize.maxWidth}
                        aria-valuenow={Math.round(resize.width)}
                        tabIndex={0}
                        onPointerDown={resize.handleStart}
                        onPointerMove={resize.handleMove}
                        onPointerUp={resize.handleEnd}
                        onPointerCancel={resize.handleEnd}
                        onLostPointerCapture={resize.handleEnd}
                        onKeyDown={resize.handleKeyDown}
                        className={`absolute inset-y-0 z-20 w-2 touch-none cursor-col-resize select-none hover:bg-blue-400/25 focus:outline-none focus-visible:bg-blue-400/40 ${props.placement === "left" ? "-right-1" : "-left-1"} ${resize.isResizing ? "bg-blue-400/40" : ""}`}
                    />
                ) : null}
            </aside>
            {!isPersistent && isOpen ? (
                <div
                    className="fixed inset-0 z-[70] bg-black/60"
                    role="dialog"
                    aria-modal="true"
                    aria-label={props.label}
                    onMouseDown={(event) => {
                        if (event.target === event.currentTarget) {
                            onClose();
                        }
                    }}
                >
                    <aside
                        ref={drawerRef}
                        id={drawerId}
                        tabIndex={-1}
                        className={cn(
                            "absolute flex h-full w-72 max-w-[85vw] flex-col overflow-y-auto border-slate-700 bg-surface shadow-2xl shadow-black/50",
                            edgeClass,
                            borderClass,
                            !props.flush && "p-3",
                        )}
                    >
                        <div
                            className={cn(
                                "mb-2 flex",
                                props.placement === "left"
                                    ? "justify-end"
                                    : "justify-start",
                                props.flush && "px-3 pt-3",
                            )}
                        >
                            <IconButton
                                type="button"
                                label={`Close ${props.label.toLowerCase()}`}
                                tooltip={false}
                                variant="ghost"
                                size="md"
                                onClick={onClose}
                            >
                                <X className="h-4 w-4" />
                            </IconButton>
                        </div>
                        {props.children}
                    </aside>
                </div>
            ) : null}
        </>
    );
}

/** Shares edge-aware resizing while reserving space for the central route on smaller screens. */
function useSideMenuResize(placement: "left" | "right") {
    const [widths, setWidths] = useAtom(sidebarWidthsAtom);
    const [viewportWidth, setViewportWidth] = React.useState(window.innerWidth);
    const [isResizing, setIsResizing] = React.useState(false);

    const drag = React.useRef<{
        pointerId: number;
        startX: number;
        startWidth: number;
    } | null>(null);

    const maxWidth = Math.max(180, Math.min(480, viewportWidth * 0.35));
    const storedWidth = widths[placement];

    const width = Math.max(
        180,
        Math.min(maxWidth, Number.isFinite(storedWidth) ? storedWidth : 224),
    );

    const direction = placement === "left" ? 1 : -1;

    React.useEffect(() => {
        /** Re-clamps rendered widths when switching viewport sizes without losing the preference. */
        const handleViewportResize = () => setViewportWidth(window.innerWidth);
        window.addEventListener("resize", handleViewportResize);

        return () => window.removeEventListener("resize", handleViewportResize);
    }, []);

    /** Uses one bound for pointer and keyboard input so neither can consume the entire shell. */
    const updateWidth = (next: number) => {
        setWidths((current) => ({
            ...current,
            [placement]: Math.max(180, Math.min(maxWidth, next)),
        }));
    };

    /** Captures the pointer so dragging continues after leaving the narrow edge handle. */
    const handleStart = (event: React.PointerEvent<HTMLDivElement>) => {
        if (event.button !== 0 || drag.current) {
            return;
        }

        drag.current = {
            pointerId: event.pointerId,
            startX: event.clientX,
            startWidth: width,
        };
        event.currentTarget.setPointerCapture(event.pointerId);
        event.currentTarget.focus();
        setIsResizing(true);
        event.preventDefault();
    };

    /** Mirrors drag direction on the right edge to keep movement attached to the pointer. */
    const handleMove = (event: React.PointerEvent<HTMLDivElement>) => {
        const active = drag.current;

        if (active?.pointerId === event.pointerId) {
            updateWidth(
                active.startWidth + (event.clientX - active.startX) * direction,
            );
        }
    };

    /** Clears drag feedback even when capture is canceled by the browser. */
    const handleEnd = (event: React.PointerEvent<HTMLDivElement>) => {
        if (drag.current?.pointerId !== event.pointerId) {
            return;
        }

        drag.current = null;
        setIsResizing(false);

        if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
        }
    };

    /** Gives a focused separator the same physical edge movement as dragging. */
    const handleKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
        if (
            shouldIgnoreKeyboardShortcut(event.nativeEvent, { shift: true }) ||
            (event.key !== "ArrowLeft" && event.key !== "ArrowRight")
        ) {
            return;
        }

        event.preventDefault();
        updateWidth(
            width + (event.key === "ArrowRight" ? 24 : -24) * direction,
        );
    };

    return {
        width,
        maxWidth,
        isResizing,
        handleStart,
        handleMove,
        handleEnd,
        handleKeyDown,
    };
}
