import { createCn } from "cn/config";

/**
 * Combines class names for components. Callers can write conditional classes as objects or
 * arrays instead of hand-built template strings, and Tailwind conflicts are resolved so a
 * later class, such as one passed in through a className prop, overrides the component's
 * default instead of racing it in the generated stylesheet.
 */
export const cn = createCn({
    extend: {
        classGroups: {
            // The merger only knows Tailwind's default theme, so it reads the custom
            // `text-code` token from styles.css as a text color. A later color such as
            // `text-slate-200` then removes it and diffs fall back to the browser's 16px,
            // so register it as a font size, the group it really belongs to.
            "font-size": [{ text: ["code"] }],
        },
    },
});
