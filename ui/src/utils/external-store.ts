/**
 * Lets a plain class publish immutable snapshots that React reads with
 * `useSyncExternalStore`. Owners of long-lived resources (sockets, queues, emulators) extend
 * it so their state lives outside any component, while components only subscribe to it.
 */
export class ExternalStore<T> {
    private snapshot: T;
    private readonly listeners = new Set<() => void>();

    /** Starts from an initial snapshot so readers never see an unset state. */
    constructor(initial: T) {
        this.snapshot = initial;
    }

    /** Arrow property so it can be handed to `useSyncExternalStore` without binding. */
    readonly subscribe = (listener: () => void): (() => void) => {
        this.listeners.add(listener);

        return () => {
            this.listeners.delete(listener);
        };
    };

    /** Returns the current snapshot; its identity only changes when the state does. */
    readonly getSnapshot = (): T => this.snapshot;

    /** Replaces the snapshot and wakes subscribers; a set tolerates one unsubscribing mid-loop. */
    protected publish(next: T): void {
        this.snapshot = next;

        for (const listener of this.listeners) {
            listener();
        }
    }
}
