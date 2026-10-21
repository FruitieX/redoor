import * as React from "react";
import { X } from "lucide-react";
import { useAtom } from "jotai";
import { IconButton } from "#ui/components/icon-button";
import { isSortableDragActive } from "#ui/components/sortable-list";
import { sidebarWidthsAtom, usePersistentSideMenus } from "#ui/side-menu-state";
import { shouldIgnoreKeyboardShortcut } from "#ui/utils/keyboard";

const focusableSelector =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Provides the shared wide-screen sidebar and responsive drawer behavior for either edge. */
export function SideMenu(props: {
    placement: "left" | "right";
    label: string;
    drawerId: string;
    isOpen: boolean;
    triggerRef: React.RefObject<HTMLButtonElement | null>;
    onClose: () => void;
    children: React.ReactNode;
}) {
    const drawerRef = React.useRef<HTMLElement>(null);
    const resize = useSideMenuResize(props.placement);
    const panelId = React.useId();

    React.useEffect(() => {
        if (!props.isOpen) {
            return;
        }

        const drawer = drawerRef.current;

        const focusable =
            drawer?.querySelectorAll<HTMLElement>(focusableSelector);

        focusable?.item(0).focus();

        /** Keeps keyboard interaction inside the modal drawer and supports conventional dismissal. */
        const handleDrawerKeyDown = (event: KeyboardEvent) => {
            if (event.key === "Escape") {
                // The sortable sensor cancels in a later bubble listener, so a live drag must win.
                if (event.defaultPrevented || isSortableDragActive()) {
                    return;
                }

                event.preventDefault();
                props.onClose();

                return;
            }

            if (event.key !== "Tab" || !drawer) {
                return;
            }

            const entries = [
                ...drawer.querySelectorAll<HTMLElement>(focusableSelector),
            ];

            const first = entries[0];
            const last = entries.at(-1);

            if (!first || !last) {
                event.preventDefault();
                drawer.focus();

                return;
            }

            if (event.shiftKey && document.activeElement === first) {
                event.preventDefault();
                last.focus();
            } else if (!event.shiftKey && document.activeElement === last) {
                event.preventDefault();
                first.focus();
            }
        };

        document.addEventListener("keydown", handleDrawerKeyDown);

        return () => {
            document.removeEventListener("keydown", handleDrawerKeyDown);
            props.triggerRef.current?.focus();
        };
    }, [props.isOpen, props.onClose, props.triggerRef]);

    const isPersistent = usePersistentSideMenus();
    const borderClass = props.placement === "left" ? "border-r" : "border-l";
    const edgeClass = props.placement === "left" ? "left-0" : "right-0";

    return (
        <>
            <aside
                id={panelId}
                aria-label={props.label}
                style={{ width: resize.width }}
                className={`${isPersistent ? "flex" : "hidden"} relative min-w-0 shrink-0 flex-col ${borderClass} border-slate-800 bg-[#0f1218] p-3`}
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
            {!isPersistent && props.isOpen ? (
                <div
                    className="fixed inset-0 z-[70] bg-black/60"
                    role="dialog"
                    aria-modal="true"
                    aria-label={props.label}
                    onMouseDown={(event) => {
                        if (event.target === event.currentTarget) {
                            props.onClose();
                        }
                    }}
                >
                    <aside
                        ref={drawerRef}
                        id={props.drawerId}
                        tabIndex={-1}
                        className={`absolute ${edgeClass} flex h-full w-72 max-w-[85vw] flex-col overflow-y-auto ${borderClass} border-slate-700 bg-[#11141b] p-3 shadow-2xl shadow-black/50`}
                    >
                        <div
                            className={`mb-2 flex ${props.placement === "left" ? "justify-end" : "justify-start"}`}
                        >
                            <IconButton
                                type="button"
                                label={`Close ${props.label.toLowerCase()}`}
                                tooltip={false}
                                onClick={props.onClose}
                                className="rounded p-2 text-slate-400 hover:bg-white/10 hover:text-slate-100"
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
