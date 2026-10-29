/**
 * A file name query as typed into a diff list's filter. Every word must appear in a path for
 * it to stay listed, and a word with a leading dash must not appear, so `-test` hides the
 * test files of a change set. Quotes keep a word literal, so `"-test"` looks for names that
 * contain a dash and `"two words"` keeps its space. Matching ignores case, because file name
 * conventions mix it and a filter is typed quickly.
 */
export class FileNameFilter {
    /** Lowercased words every listed path must contain. */
    readonly include: readonly string[];
    /** Lowercased words no listed path may contain. */
    readonly exclude: readonly string[];

    private constructor(include: string[], exclude: string[]) {
        this.include = include;
        this.exclude = exclude;
    }

    /**
     * Reads a query into its words. A lone dash or empty quotes add nothing, so the list does
     * not jump while `-test` or `"…"` is still being typed, and an unclosed quote runs to the
     * end of the query for the same reason.
     */
    static parse(query: string): FileNameFilter {
        const include: string[] = [];
        const exclude: string[] = [];
        let index = 0;

        while (index < query.length) {
            if (/\s/.test(query.charAt(index))) {
                index += 1;
                continue;
            }

            const negated = query.charAt(index) === "-";

            if (negated) {
                index += 1;
            }

            let word: string;

            if (query.charAt(index) === '"') {
                const closing = query.indexOf('"', index + 1);
                const end = closing === -1 ? query.length : closing;
                word = query.slice(index + 1, end);
                index = end + 1;
            } else {
                const start = index;

                while (
                    index < query.length &&
                    !/\s/.test(query.charAt(index))
                ) {
                    index += 1;
                }

                word = query.slice(start, index);
            }

            if (word === "") {
                continue;
            }

            (negated ? exclude : include).push(word.toLowerCase());
        }

        return new FileNameFilter(include, exclude);
    }

    /** Whether the query has any word, so views can tell a filtered list from a full one. */
    get isActive(): boolean {
        return this.include.length > 0 || this.exclude.length > 0;
    }

    /** Whether a path contains every included word and none of the excluded ones. */
    matches(path: string): boolean {
        const lowerPath = path.toLowerCase();

        return (
            this.include.every((word) => lowerPath.includes(word)) &&
            !this.exclude.some((word) => lowerPath.includes(word))
        );
    }
}
