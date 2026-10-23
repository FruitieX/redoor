import { createFileRoute } from "@tanstack/react-router";
import { Home } from "lucide-react";

import { LinkButton } from "#ui/components/link-button";

/**
 * Catches addresses no page owns. Handling them below the `_app` layout keeps the menus
 * around the message, so a mistyped URL leaves the user one click from somewhere useful.
 */
export const Route = createFileRoute("/_app/$")({
    component: PageNotFound,
});

/** Tells the user the page does not exist and offers the way back to the server home. */
function PageNotFound() {
    return (
        <div className="flex h-full items-center justify-center p-8">
            <section className="max-w-lg text-center">
                <h1 className="text-2xl font-semibold text-slate-100">
                    Page not found
                </h1>
                <p className="mt-2 text-slate-400">
                    Nothing lives at this address.
                </p>
                <LinkButton to="/" className="mt-6">
                    <Home className="h-4 w-4" aria-hidden="true" />
                    Server home
                </LinkButton>
            </section>
        </div>
    );
}
