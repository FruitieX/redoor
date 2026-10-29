import { expect, test } from "vitest";

import { documentTitle } from "#ui/components/shell/document-title";

const serverInfo = { hostname: "box", version: "1.4.4" };

test("titles pages outside the file browser with the server only", () => {
    // Without a browsed path there is no entry or agent to name, only the server.
    expect(documentTitle(serverInfo, null)).toBe("box | Redoor 1.4.4");
});

test("titles the file browser with the entry base name and agent", () => {
    // The base name keeps the distinguishing part visible in narrow browser tabs.
    expect(
        documentTitle(serverInfo, {
            deviceName: "laptop",
            path: "/home/user/notes.txt",
        }),
    ).toBe("notes.txt@laptop | box | Redoor 1.4.4");
});

test("titles the browser root as the root directory", () => {
    // The root has no base name, so the slash itself identifies it.
    expect(documentTitle(serverInfo, { deviceName: "laptop", path: "/" })).toBe(
        "/@laptop | box | Redoor 1.4.4",
    );
});

test("titles a directory with a trailing slash by its own name", () => {
    // A trailing slash must not leave the title with an empty base name.
    expect(
        documentTitle(serverInfo, { deviceName: "laptop", path: "/srv/www/" }),
    ).toBe("www@laptop | box | Redoor 1.4.4");
});
