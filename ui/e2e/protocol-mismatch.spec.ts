import http from "node:http";
import { expect, test } from "@playwright/test";

import { WEB_BASE_URL } from "./helpers";

/** Agent token the Playwright server is configured with. */
const AGENT_TOKEN = "test-agent-token";

/**
 * Attempts an agent websocket upgrade that offers an unknown protocol, the way an agent of a
 * different release would, and resolves with the HTTP status the server answered.
 */
function upgradeWithProtocol(protocol: string, version: string) {
    const url = new URL("/api/v1/agent/ws", WEB_BASE_URL);

    return new Promise<number>((resolve, reject) => {
        const request = http.request(url, {
            headers: {
                Connection: "Upgrade",
                Upgrade: "websocket",
                "Sec-WebSocket-Version": "13",
                "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==",
                "Sec-WebSocket-Protocol": protocol,
                "X-Redoor-Agent-Version": version,
                Authorization: `Bearer ${AGENT_TOKEN}`,
            },
        });

        request.on("response", (response) => {
            response.resume();
            resolve(response.statusCode ?? 0);
        });
        request.on("upgrade", (_response, socket) => {
            socket.destroy();
            reject(new Error("the server accepted an unknown protocol"));
        });
        request.on("error", reject);
        request.end();
    });
}

test.describe("Protocol mismatches", () => {
    test("lists agents refused for speaking another protocol", async ({
        page,
    }) => {
        const version = `0.0.0-mismatch-${Date.now()}`;

        // An agent of another protocol generation is refused before any socket exists.
        expect(await upgradeWithProtocol("redoor.v1", version)).toBe(426);

        await page.goto(`${WEB_BASE_URL}/agents`);

        const refused = page.getByRole("region", {
            name: "Refused agent connections",
        });

        // The refused agent never registers, so this list is the only place the UI shows it.
        await expect(refused).toBeVisible();
        // The agent's own version identifies which installation needs the upgrade.
        await expect(
            refused.getByRole("row").filter({ hasText: version }),
        ).toContainText("redoor.v1");
        // The explanation names the protocol the agent has to speak instead.
        await expect(refused).toContainText("redoor.v2");
    });
});
