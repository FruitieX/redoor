/**
 * Describes where a filesystem path lives in the router, so links and navigations carry the
 * typed `to` and `params` instead of a URL built by hand. The router encodes each path
 * segment itself and keeps the slashes readable. Spread the result into a `Link` or
 * `navigate` call and add `search` when the destination needs a view. Use
 * `Agent.getBrowserUrl` only where a real string is required, such as a copied link or a
 * native `href`.
 */
export function browserRoute(agentId: string, path: string) {
    if (!path.startsWith("/")) {
        throw new Error("Filesystem path must be absolute");
    }

    return {
        to: "/agents/$agentId/browser/$",
        params: { agentId, _splat: path.slice(1) },
    } as const;
}
