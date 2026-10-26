import type { SupportedSyntaxLanguage } from "#ui/utils/editor-language";
import {
    HIGHLIGHT_LANGUAGE_NAMES,
    syntaxHighlighter,
} from "#ui/utils/syntax-highlighting";

/**
 * A run of highlighted text. Tokens are flat rather than a tree so a caller can cut them at
 * any character offset, which is what inline diff segments need.
 */
export interface HighlightToken {
    text: string;
    /** The CSS classes of the token, or null for plain text. */
    className: string | null;
    /**
     * The token's innermost grammar kind, such as "keyword", exposed as `data-token` so tests
     * and tools can find tokens without depending on the highlighter's class names.
     */
    kind: string | null;
}

const HTML_ENTITIES = new Map([
    ["&amp;", "&"],
    ["&lt;", "<"],
    ["&gt;", ">"],
    ["&quot;", '"'],
    ["&#x27;", "'"],
    ["&#39;", "'"],
]);

/** Reverses the escaping highlight.js applies to source text. */
function decodeEntities(text: string): string {
    return text.replace(/&(?:amp|lt|gt|quot|#x27|#39);/g, (entity) => {
        return HTML_ENTITIES.get(entity) ?? entity;
    });
}

/**
 * Flattens highlight.js HTML into tokens. Nested scopes keep all their classes, so rules
 * such as `.hljs-meta .hljs-string` still match a single span.
 */
function tokensFromHighlightHtml(html: string): HighlightToken[] {
    const tokens: HighlightToken[] = [];
    const scopes: string[] = [];
    const pattern = /<span class="([^"]*)">|<\/span>|([^<]+)/g;

    for (const match of html.matchAll(pattern)) {
        if (match[1] !== undefined) {
            scopes.push(match[1]);
        } else if (match[2] !== undefined) {
            const innermost = scopes.at(-1)?.split(" ")[0];

            tokens.push({
                text: decodeEntities(match[2]),
                className: scopes.length === 0 ? null : scopes.join(" "),
                kind: innermost?.replace(/^hljs-/, "") ?? null,
            });
        } else {
            scopes.pop();
        }
    }

    return tokens;
}

/**
 * Highlights a block of source and returns the tokens of each line. Highlighting a block at
 * once, instead of every line alone, keeps multi-line strings and comments coloured right.
 */
export function highlightLines(
    code: string,
    language: SupportedSyntaxLanguage,
): HighlightToken[][] {
    const html = syntaxHighlighter.highlight(code, {
        language: HIGHLIGHT_LANGUAGE_NAMES[language],
        ignoreIllegals: true,
    }).value;

    const lines: HighlightToken[][] = [[]];

    for (const token of tokensFromHighlightHtml(html)) {
        const parts = token.text.split("\n");

        parts.forEach((part, index) => {
            if (index > 0) {
                lines.push([]);
            }

            if (part !== "") {
                lines.at(-1)?.push({ ...token, text: part });
            }
        });
    }

    return lines;
}
