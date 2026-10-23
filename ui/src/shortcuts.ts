import * as React from "react";

import { isTypingTarget } from "#ui/utils/keyboard";

/**
 * Chooses the DOM phase a binding listens in. Capture runs before the focused element, so
 * it is the only place to claim a key from CodeMirror, Vim or the terminal. Bubble runs
 * after the focused element and the page's own handlers, which is where ordinary page
 * shortcuts belong so a control that handled the key itself keeps it.
 */
export type ShortcutPhase = "capture" | "bubble";

/**
 * Orders bindings that could answer the same key. The higher tier is offered the event
 * first, so a layer that sits on top of the page (an open menu, a drag in progress) wins
 * over what lies beneath it without relying on registration order.
 */
export const shortcutPriority = {
    /** Passive watchers that never claim a key, so they must see it before any claimant. */
    observer: 1000,
    /** Open menus and drawers: dismissing the topmost layer must win over everything below. */
    modal: 400,
    /** A gesture in progress, such as a keyboard sort, that must not trigger page shortcuts. */
    interaction: 300,
    /** Keys the editor owns, such as save and find, which also work with focus outside it. */
    editor: 200,
    /** Shortcuts of the whole application, such as opening the terminal or the search. */
    app: 100,
    /** Shortcuts of the page on screen, such as filtering or creating files. */
    view: 0,
} as const;

/**
 * Says what happens to the event when a binding handles it:
 * - `prevent` cancels the browser's default action; other DOM listeners still see the key.
 * - `swallow` also stops propagation, so CodeMirror, Vim or the terminal never see the key.
 * - `shield` only stops propagation and keeps the default action, to hide a key from
 *   handlers beneath without taking the browser's own behaviour away.
 */
export type ShortcutEffect = "prevent" | "swallow" | "shield";

/** Describes one keyboard command and the conditions under which it owns a key press. */
export type ShortcutBinding = {
    /** Identifies the binding in diagnostics, since several share one key. */
    name: string;
    phase: ShortcutPhase;
    priority: number;
    /** Selects the key presses this binding is interested in, including modifiers. */
    match: (event: KeyboardEvent) => boolean;
    /** Keeps the binding out of text controls, the terminal and the editor, where typing wins. */
    skipTypingTargets?: boolean;
    /** Defaults to `prevent`. */
    effect?: ShortcutEffect;
    /**
     * Runs the command. Returning false means the key was only observed, or the command did
     * not apply after all, so the event moves on to the next binding unclaimed.
     */
    run: (event: KeyboardEvent) => boolean | void;
};

/** Pairs a binding with its arrival order, which breaks ties between equal priorities. */
type RegisteredBinding = {
    binding: ShortcutBinding;
    order: number;
};

/**
 * The single owner of the window's keyboard shortcuts. Every global shortcut registers here
 * instead of adding its own `keydown` listener, so which command wins a key is decided by
 * explicit phase and priority, with one rule for consumption: the first binding that
 * handles a key press claims it, and nothing after it, in either phase, is offered it.
 * The window listeners exist only while something is registered.
 */
export class ShortcutRegistry {
    private readonly registered = new Set<RegisteredBinding>();
    private readonly claimed = new WeakSet<KeyboardEvent>();
    private nextOrder = 0;
    private isAttached = false;

    /** Takes the event target so tests can use a detached window and the app uses the real one. */
    constructor(private readonly target: EventTarget) {}

    /** Adds a binding and returns the function that removes it again. */
    register(binding: ShortcutBinding): () => void {
        const entry: RegisteredBinding = { binding, order: this.nextOrder };
        this.nextOrder += 1;
        this.registered.add(entry);
        this.attach();

        return () => {
            this.registered.delete(entry);

            if (this.registered.size === 0) {
                this.detach();
            }
        };
    }

    /**
     * Offers one key press to the bindings of a phase, from the highest priority down, and
     * applies the winner's effect. Public so tests can drive it without a DOM listener.
     */
    dispatch(event: KeyboardEvent, phase: ShortcutPhase): void {
        if (this.claimed.has(event)) {
            return;
        }

        const candidates = [...this.registered]
            .filter((entry) => entry.binding.phase === phase)
            .sort(
                (left, right) =>
                    right.binding.priority - left.binding.priority ||
                    left.order - right.order,
            );

        for (const entry of candidates) {
            if (this.tryBinding(entry.binding, event)) {
                return;
            }
        }
    }

    /** Runs one binding if it applies and reports whether it claimed the key press. */
    private tryBinding(
        binding: ShortcutBinding,
        event: KeyboardEvent,
    ): boolean {
        if (
            binding.skipTypingTargets === true &&
            isTypingTarget(event.target)
        ) {
            return false;
        }

        if (!binding.match(event) || binding.run(event) === false) {
            return false;
        }

        this.claimed.add(event);
        const effect = binding.effect ?? "prevent";

        if (effect !== "shield") {
            event.preventDefault();
        }

        if (effect !== "prevent") {
            event.stopPropagation();
        }

        return true;
    }

    /** Listens in both phases through arrow properties so removal finds the same functions. */
    private readonly handleCapture = (event: Event) => {
        if (event instanceof KeyboardEvent) {
            this.dispatch(event, "capture");
        }
    };

    private readonly handleBubble = (event: Event) => {
        if (event instanceof KeyboardEvent) {
            this.dispatch(event, "bubble");
        }
    };

    /** Starts listening when the first binding arrives, so an idle registry costs nothing. */
    private attach(): void {
        if (this.isAttached) {
            return;
        }

        this.isAttached = true;
        this.target.addEventListener("keydown", this.handleCapture, true);
        this.target.addEventListener("keydown", this.handleBubble, false);
    }

    /** Stops listening once nothing is registered, so a discarded registry leaks no listener. */
    private detach(): void {
        this.isAttached = false;
        this.target.removeEventListener("keydown", this.handleCapture, true);
        this.target.removeEventListener("keydown", this.handleBubble, false);
    }
}

/** Supplies the application's registry to components, so tests can give them their own. */
export const ShortcutRegistryContext =
    React.createContext<ShortcutRegistry | null>(null);

/** Reads the registry provided at the application root. */
function useShortcutRegistry(): ShortcutRegistry {
    const registry = React.useContext(ShortcutRegistryContext);

    if (registry === null) {
        throw new Error("Missing ShortcutRegistryContext provider");
    }

    return registry;
}

/**
 * Registers a shortcut for as long as the component is mounted and `enabled` holds. The
 * registration depends only on the binding's fixed properties; `match` and `run` are read
 * through a ref, so they always see the latest props and state without unregistering and
 * registering again on every render.
 */
export function useShortcut(binding: ShortcutBinding, enabled = true): void {
    const registry = useShortcutRegistry();
    const latest = React.useRef(binding);

    React.useLayoutEffect(() => {
        latest.current = binding;
    });

    React.useEffect(() => {
        if (!enabled) {
            return;
        }

        return registry.register({
            name: binding.name,
            phase: binding.phase,
            priority: binding.priority,
            skipTypingTargets: binding.skipTypingTargets,
            effect: binding.effect,
            match: (event) => latest.current.match(event),
            run: (event) => latest.current.run(event),
        });
    }, [
        registry,
        enabled,
        binding.name,
        binding.phase,
        binding.priority,
        binding.skipTypingTargets,
        binding.effect,
    ]);
}
