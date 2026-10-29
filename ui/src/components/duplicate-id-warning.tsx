import { Copy } from "lucide-react";

import type { DuplicateIdWarning as DuplicateIdWarningData } from "#bindings/DuplicateIdWarning";

/**
 * Explains why a device keeps refusing one of its machines. A cloned data dir (home directory,
 * VM or container image) copies the agent's id file, so several machines claim one device;
 * the router keeps the current connection and refuses the others. Without this notice the
 * refused clone would only be visible in the server log. The hostnames tell the operator
 * which machines are involved, and the hint names the one file that gives a clone its own id.
 */
export function DuplicateIdWarning(props: {
    warning: DuplicateIdWarningData;
    /** Fits the notice into a table cell instead of a page-width banner. */
    compact?: boolean;
}) {
    const hostnames =
        props.warning.hostnames.length > 0
            ? props.warning.hostnames.join(", ")
            : "unknown";

    return (
        <div
            role="alert"
            aria-label="Duplicate device id"
            className={
                props.compact
                    ? "space-y-1 text-amber-300"
                    : "flex gap-3 rounded-lg border border-amber-700/60 bg-amber-950/30 p-4 text-sm text-amber-200"
            }
        >
            {props.compact ? null : (
                <Copy
                    className="mt-0.5 h-5 w-5 shrink-0 text-amber-400"
                    aria-hidden="true"
                />
            )}
            <div className="min-w-0 space-y-1">
                <p className="font-semibold">
                    Several machines are using this device&apos;s id
                </p>
                <p className="break-words">Hostnames: {hostnames}</p>
                <p className="break-words text-slate-300">
                    Delete{" "}
                    <code className="font-mono text-amber-100">
                        &lt;data_dir&gt;/agent-id
                    </code>{" "}
                    on one of the machines and restart its agent to give it its
                    own id.
                </p>
            </div>
        </div>
    );
}
