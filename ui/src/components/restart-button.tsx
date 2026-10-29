import * as React from "react";
import { useMutation } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import type { RestartResponse } from "#bindings/RestartResponse";

import { Button } from "./button";
import { ConfirmationDialog } from "./confirmation-dialog";
import { getErrorMessage } from "#ui/utils/error";

/** Normalizes failures from both API requests and readiness polling. */
function restartErrorMessage(cause: unknown): string {
    return getErrorMessage(cause, "Restart failed");
}

/** Provides the same confirmed restart workflow for server and agent process homes. */
export function RestartButton(props: {
    target: string;
    description: React.ReactNode;
    restart: () => Promise<RestartResponse>;
    waitUntilReady: () => Promise<void>;
    className?: string;
    ariaLabel?: string;
}) {
    const [isOpen, setIsOpen] = React.useState(false);

    return (
        <>
            <Button
                type="button"
                variant={props.className ? "subtle" : "secondary"}
                aria-label={props.ariaLabel}
                onClick={() => setIsOpen(true)}
                className={props.className}
            >
                <RefreshCw className="h-4 w-4" aria-hidden="true" />
                Restart
            </Button>
            <RestartDialog
                isOpen={isOpen}
                onClose={() => setIsOpen(false)}
                target={props.target}
                description={props.description}
                restart={props.restart}
                waitUntilReady={props.waitUntilReady}
            />
        </>
    );
}

/**
 * Confirms and runs a restart for a caller that opens it from its own control. Callers that
 * open it from a menu keep it outside that menu, so closing the menu does not unmount the
 * confirmation while the restart is still running.
 */
export function RestartDialog(props: {
    isOpen: boolean;
    onClose: () => void;
    target: string;
    description: React.ReactNode;
    restart: () => Promise<RestartResponse>;
    waitUntilReady: () => Promise<void>;
}) {
    const restartMutation = useMutation({
        mutationFn: async () => {
            await props.restart();
            await props.waitUntilReady();
        },
        onSuccess: () => {
            props.onClose();
        },
    });

    /** Keeps the dialog up while the restart runs, and drops a stale error for the next open. */
    const close = () => {
        if (restartMutation.isPending) return;
        restartMutation.reset();
        props.onClose();
    };

    return (
        <ConfirmationDialog
            isOpen={props.isOpen}
            title={`Restart ${props.target}?`}
            description={props.description}
            confirmLabel="Restart"
            busyLabel="Restarting..."
            isBusy={restartMutation.isPending}
            errorMessage={
                restartMutation.isError
                    ? restartErrorMessage(restartMutation.error)
                    : null
            }
            onClose={close}
            onConfirm={() => restartMutation.mutate()}
        />
    );
}
