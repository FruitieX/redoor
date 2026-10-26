import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

/**
 * Combines class names for components. clsx lets callers write conditional classes as objects
 * or arrays instead of hand-built template strings, and twMerge then resolves Tailwind
 * conflicts so a later class, such as one passed in through a className prop, overrides the
 * component's default instead of racing it in the generated stylesheet.
 */
export function cn(...inputs: ClassValue[]): string {
    return twMerge(clsx(inputs));
}
