import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

/**
 * tailwind-merge only knows Tailwind's default theme, so it reads custom `text-*` tokens
 * from styles.css as text colors and drops them when a color such as `text-slate-200`
 * follows. Registering them as font sizes keeps the shared code size on diffs.
 */
const twMerge = extendTailwindMerge({
    extend: {
        classGroups: {
            "font-size": [{ text: ["code"] }],
        },
    },
});

/**
 * Combines class names for components. clsx lets callers write conditional classes as objects
 * or arrays instead of hand-built template strings, and twMerge then resolves Tailwind
 * conflicts so a later class, such as one passed in through a className prop, overrides the
 * component's default instead of racing it in the generated stylesheet.
 */
export function cn(...inputs: ClassValue[]): string {
    return twMerge(clsx(inputs));
}
