import type * as React from "react";
import { Check, Copy } from "lucide-react";
import { shortCommitId } from "#ui/components/browser/git-parts";
import { useCopyFeedback } from "#ui/components/copyable-code-row";
import { IconButton } from "#ui/components/icon-button";
import { cn } from "#ui/utils/cn";

/** What a Git value names, which decides how it is shortened and how the copy is worded. */
export type GitRefKind = "commit" | "blob" | "branch" | "tag" | "ref";

/** Hashes are worded as hashes and names as names, so the tooltip says what lands in the terminal. */
const nouns = {
    commit: "commit hash",
    blob: "blob hash",
    branch: "branch name",
    tag: "tag name",
    ref: "ref name",
} satisfies Record<GitRefKind, string>;

/**
 * A commit hash or ref name with a copy button beside it. People need these values in a
 * terminal, and selecting a short monospace id by hand is fiddly, especially on phones.
 * Hashes are shown short but always copied in full, so the copy is never ambiguous. The
 * visible part can be a link; the button sits beside it because a button cannot nest in one.
 */
export function CopyableGitRef(props: {
    kind: GitRefKind;
    /** The whole value put on the clipboard, such as the full commit hash. */
    value: string;
    /** The visible value, such as a link to the commit. Defaults to the short hash or the name. */
    children?: React.ReactNode;
    /** Lets the wrapper itself be a pill or a truncating flex item. */
    className?: string;
}) {
    const { isCopied, copy } = useCopyFeedback(props.value);
    const isHash = props.kind === "commit" || props.kind === "blob";
    const display = isHash ? shortCommitId(props.value) : props.value;
    const noun = nouns[props.kind];

    return (
        <span
            className={cn(
                "inline-flex min-w-0 max-w-full items-center gap-0.5",
                props.className,
            )}
        >
            {props.children ?? (
                <code className="truncate font-mono">{display}</code>
            )}
            <IconButton
                type="button"
                // Names the value so rows of copy buttons stay distinguishable to screen readers.
                label={isCopied ? "Copied!" : `Copy ${noun} ${display}`}
                tooltip={isCopied ? "Copied!" : `Copy ${noun} ${props.value}`}
                tooltipClassName="shrink-0"
                onClick={() => {
                    void copy();
                }}
                className={cn(
                    "shrink-0 rounded p-1 transition-colors",
                    isCopied
                        ? "text-emerald-300"
                        : "text-current opacity-60 hover:bg-tint hover:opacity-100",
                )}
            >
                {isCopied ? (
                    <Check className="h-3.5 w-3.5" aria-hidden="true" />
                ) : (
                    <Copy className="h-3.5 w-3.5" aria-hidden="true" />
                )}
            </IconButton>
        </span>
    );
}
