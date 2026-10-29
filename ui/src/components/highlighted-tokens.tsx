import React from "react";
import type { HighlightToken } from "#ui/utils/code-highlight";

/**
 * Renders highlighted tokens as React nodes instead of an HTML string, so untrusted source
 * never passes through `dangerouslySetInnerHTML`. Plain text stays bare to keep large diffs
 * light; coloured tokens carry `data-token` so tests can find them without class names.
 */
export function HighlightedTokens(props: { tokens: HighlightToken[] }) {
    return props.tokens.map((token, index) =>
        token.className === null ? (
            <React.Fragment key={index}>{token.text}</React.Fragment>
        ) : (
            <span
                key={index}
                className={token.className}
                data-token={token.kind ?? undefined}
            >
                {token.text}
            </span>
        ),
    );
}

/**
 * Renders a highlighted block line by line, putting back the newlines the highlighter split
 * on, for code shown as one `<code>` element such as a Markdown fence.
 */
export function HighlightedLines(props: { lines: HighlightToken[][] }) {
    return props.lines.map((tokens, index) => (
        <React.Fragment key={index}>
            {index === 0 ? null : "\n"}
            <HighlightedTokens tokens={tokens} />
        </React.Fragment>
    ));
}
