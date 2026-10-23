/** Reads the last path segment as a display name, using "/" for the filesystem root. */
export function getBaseName(path: string): string {
    return path.split("/").filter(Boolean).pop() ?? "/";
}

/**
 * Swaps the last path segment so a conflicting destination can be renamed while it stays in
 * the directory the user chose. Works on relative paths too, since Sync accepts typed paths.
 */
export function replaceBaseName(path: string, name: string): string {
    return path.replace(/[^/]+\/*$/, name);
}

/**
 * Expands the agent's home shorthand so a typed `~` or `~/dir` resolves against that
 * device's own home. Paths without the shorthand, and agents whose home is not known yet,
 * pass through unchanged.
 */
export function expandHomePath(home: string | null, path: string): string {
    if (home === null || (path !== "~" && !path.startsWith("~/"))) {
        return path;
    }

    if (path === "~") {
        return home;
    }

    return home === "/"
        ? `/${path.slice(2)}`
        : `${home.replace(/\/+$/, "")}/${path.slice(2)}`;
}

/** Describes what the user is naming, so validation messages read naturally. */
export type PathComponentKind = "File" | "Directory";

/** Chooses whether a name may span several path components. */
export type PathComponentOptions = {
    /**
     * Accepts `a/b` and validates each component. Only directory creation sets this, because
     * the agent creates every missing parent, while renames, copies and file creation need a
     * single entry in the current directory.
     */
    allowNested?: boolean;
};

/**
 * Checks a name the user typed before it becomes part of a path. Rejecting `.`, `..` and
 * separators here keeps every naming dialog from escaping the directory it was opened in.
 * Returns the message to show, or null when the name is acceptable. The caller trims first.
 */
export function validatePathComponent(
    name: string,
    kind: PathComponentKind,
    options: PathComponentOptions = {},
): string | null {
    if (name === "") {
        return `${kind} name is required`;
    }

    const components = options.allowNested === true ? name.split("/") : [name];

    for (const component of components) {
        if (component === "") {
            return `${kind} name cannot have empty path components`;
        }

        if (component === "." || component === "..") {
            return `${kind} name cannot contain "." or ".." components`;
        }

        if (component.includes("/")) {
            return `${kind} name must be a single path component`;
        }
    }

    return null;
}

export function getParentPath(path: string): string | null {
    if (!path.startsWith("/")) return null;
    const normalizedPath = path.replace(/\/+$/, "");

    if (normalizedPath === "") return null;

    const lastSlashIndex = normalizedPath.lastIndexOf("/");

    if (lastSlashIndex === -1) return null;

    if (lastSlashIndex === 0) return "/";

    return normalizedPath.slice(0, lastSlashIndex);
}

export function formatSize(bytes: number): string {
    const units = ["B", "KB", "MB", "GB", "TB"];
    let value = bytes;
    let unitIndex = 0;

    while (value >= 1024 && unitIndex < units.length - 1) {
        value /= 1024;
        unitIndex++;
    }

    if (unitIndex === 0) {
        return `${value} ${units[unitIndex]}`;
    }

    return `${value.toFixed(1)} ${units[unitIndex]}`;
}

export function formatSpeed(bytesPerSecond: number | null): string {
    if (
        bytesPerSecond === null ||
        !Number.isFinite(bytesPerSecond) ||
        bytesPerSecond < 0
    ) {
        return "—";
    }

    return `${formatSize(bytesPerSecond)}/s`;
}
