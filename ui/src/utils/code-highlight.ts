import * as React from "react";
import {
    Language,
    LanguageDescription,
    LanguageSupport,
} from "@codemirror/language";
import { languages } from "@codemirror/language-data";
import { highlightCode, tagHighlighter, tags } from "@lezer/highlight";
import {
    syntaxLanguageFromFileName,
    syntaxLanguageFromName,
    syntaxLanguageSupport,
    type SupportedSyntaxLanguage,
} from "#ui/utils/editor-language";

/**
 * A run of highlighted text. Tokens are flat rather than a tree so a caller can cut them at
 * any character offset, which is what inline diff segments need.
 */
export interface HighlightToken {
    text: string;
    /** The CSS classes of the token, or null for plain text. */
    className: string | null;
    /**
     * The token's grammar kind, such as "keyword", exposed as `data-token` so tests and tools
     * can find tokens without depending on class names.
     */
    kind: string | null;
}

/**
 * Maps Lezer's highlight tags to `tok-*` classes. It groups tags the way the editor themes
 * (BBEdit light, Tokyo Night dark) colour them, so read-only code looks like the editor;
 * the built-in classHighlighter has no class for function names, which both themes colour.
 */
const tokenHighlighter = tagHighlighter([
    { tag: tags.keyword, class: "tok-keyword" },
    { tag: [tags.comment, tags.meta], class: "tok-comment" },
    { tag: [tags.documentMeta, tags.processingInstruction], class: "tok-meta" },
    {
        tag: [tags.string, tags.special(tags.string), tags.inserted],
        class: "tok-string",
    },
    { tag: [tags.number, tags.character], class: "tok-number" },
    {
        tag: [
            tags.changed,
            tags.annotation,
            tags.modifier,
            tags.self,
            tags.namespace,
        ],
        class: "tok-modifier",
    },
    {
        tag: [
            tags.atom,
            tags.bool,
            tags.null,
            tags.color,
            tags.constant(tags.name),
            tags.standard(tags.name),
            tags.special(tags.variableName),
        ],
        class: "tok-constant",
    },
    { tag: tags.variableName, class: "tok-variable" },
    {
        tag: [
            tags.function(tags.variableName),
            tags.function(tags.propertyName),
            tags.macroName,
        ],
        class: "tok-function",
    },
    { tag: [tags.propertyName, tags.labelName], class: "tok-property" },
    { tag: [tags.typeName, tags.className], class: "tok-type" },
    { tag: tags.tagName, class: "tok-tag" },
    {
        tag: [tags.escape, tags.regexp, tags.url, tags.link],
        class: "tok-escape",
    },
    { tag: tags.operator, class: "tok-operator" },
    { tag: tags.heading, class: "tok-heading" },
    { tag: tags.list, class: "tok-list" },
    { tag: tags.emphasis, class: "tok-emphasis" },
    { tag: tags.strong, class: "tok-strong" },
    { tag: tags.strikethrough, class: "tok-strikethrough" },
    { tag: [tags.invalid, tags.deleted], class: "tok-invalid" },
]);

/** What to highlight with: a ready grammar, one that still has to load, or none. */
export type HighlightLanguageSource = Language | LanguageDescription | null;

/** Grammars are built once per language, because the editor's setup recreates them per call. */
const supportedLanguages = new Map<SupportedSyntaxLanguage, Language>();

/** The grammar of a language the editor bundles, ready without loading. */
function supportedLanguage(language: SupportedSyntaxLanguage): Language {
    const cached = supportedLanguages.get(language);

    if (cached !== undefined) {
        return cached;
    }

    const support = syntaxLanguageSupport(language);

    const grammar =
        support instanceof LanguageSupport ? support.language : support;

    supportedLanguages.set(language, grammar);

    return grammar;
}

/**
 * Picks a grammar for a file the way the editor does. Files the editor does not know may
 * still match a lazily loaded grammar, which is fine for read-only code.
 */
export function languageForFileName(
    fileName: string,
    content: string,
): HighlightLanguageSource {
    const language = syntaxLanguageFromFileName(fileName, content);

    if (language !== undefined) {
        return supportedLanguage(language);
    }

    return LanguageDescription.matchFilename(languages, fileName);
}

/** Picks a grammar for a name such as a Markdown fence label (`ts`, `bash`, `sql`). */
export function languageForName(name: string): HighlightLanguageSource {
    const language = syntaxLanguageFromName(name);

    if (language !== undefined) {
        return supportedLanguage(language);
    }

    return LanguageDescription.matchLanguageName(languages, name, false);
}

/** One load per grammar, shared by every component waiting for it. */
const loads = new Map<LanguageDescription, Promise<void>>();

/** Starts loading a grammar once; a failed load leaves the code plain rather than broken. */
function loadDescription(description: LanguageDescription): Promise<void> {
    const existing = loads.get(description);

    if (existing !== undefined) {
        return existing;
    }

    const load = description.load().then(
        () => undefined,
        () => undefined,
    );

    loads.set(description, load);

    return load;
}

/** The grammar of a source if it is ready, otherwise null. */
function readyLanguage(source: HighlightLanguageSource): Language | null {
    if (source instanceof LanguageDescription) {
        return source.support?.language ?? null;
    }

    return source;
}

/**
 * Resolves a grammar for rendering. A grammar that has to load renders as plain text first
 * and re-renders once loaded, without suspending, so the code is readable immediately.
 */
export function useLoadedLanguage(
    source: HighlightLanguageSource,
): Language | null {
    const subscribe = React.useCallback(
        (onLoaded: () => void) => {
            if (
                !(source instanceof LanguageDescription) ||
                source.support !== undefined
            ) {
                return () => {};
            }

            let active = true;

            void loadDescription(source).then(() => {
                if (active) {
                    onLoaded();
                }
            });

            return () => {
                active = false;
            };
        },
        [source],
    );

    const snapshot = () => readyLanguage(source);

    return React.useSyncExternalStore(subscribe, snapshot, snapshot);
}

/**
 * Highlights a block of source and returns the tokens of each line. Highlighting a block at
 * once, instead of every line alone, keeps multi-line strings and comments coloured right.
 */
export function highlightLines(
    code: string,
    language: Language,
): HighlightToken[][] {
    const lines: HighlightToken[][] = [[]];
    const tree = language.parser.parse(code);

    highlightCode(
        code,
        tree,
        tokenHighlighter,
        (text, classes) => {
            const kind = classes.split(" ").at(-1)?.replace(/^tok-/, "");

            lines.at(-1)?.push({
                text,
                className: classes === "" ? null : classes,
                kind: classes === "" || kind === undefined ? null : kind,
            });
        },
        () => {
            lines.push([]);
        },
    );

    return lines;
}
