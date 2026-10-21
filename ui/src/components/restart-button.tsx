import * as React from "react";
import { useMutation } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import type { RestartResponse } from "#ui/api-client";

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

    const restartMutation = useMutation({
        mutationFn: async () => {
            await props.restart();
            await props.waitUntilReady();
        },
        onSuccess: () => {
            setIsOpen(false);
        },
    });

    const close = () => {
        if (restartMutation.isPending) return;
        setIsOpen(false);
        restartMutation.reset();
    };

    const button = (
        <Button
            type="button"
            variant={props.className ? "subtle" : "secondary"}
            aria-label={props.ariaLabel}
            onClick={() => {
                restartMutation.reset();
                setIsOpen(true);
            }}
            className={props.className}
        >
            <RefreshCw className="h-4 w-4" aria-hidden="true" />
            Restart
        </Button>
    );

    return (
        <>
            {button}
            <ConfirmationDialog
                isOpen={isOpen}
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
        </>
    );
}
