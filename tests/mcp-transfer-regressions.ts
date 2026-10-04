import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { expect, it } from "vitest";

import { type JsonObject, createMcpTestClient } from "./mcp-client";

/** Shares the live MCP fixture so regressions exercise the same server and grants as the main workflows. */
export function registerMcpTransferRegressionTests(options: {
    endpoint: string;
    baseUrl: string;
    token: () => string;
    sourceDevice: string;
    destinationDevice: string;
    sourceRoot: string;
    destinationRoot: string;
    metadataPath: string;
}): void {
    it("shares concurrent transfer admissions and rejects reuse across copy and move", async () => {
        const client = createMcpTestClient({
            endpoint: options.endpoint,
            token: options.token(),
        });
        const sourcePath = join(
            options.sourceRoot,
            "concurrent-copy-source.txt",
        );
        const destinationPath = join(
            options.destinationRoot,
            "concurrent-copy-destination.txt",
        );
        writeFileSync(sourcePath, "one transfer only");
        const arguments_: JsonObject = {
            source: { device_id: options.sourceDevice, path: sourcePath },
            destination: {
                device_id: options.destinationDevice,
                path: destinationPath,
            },
            on_existing: "error",
            idempotency_key: "concurrent-transfer",
        };
        const responses = await Promise.all(
            Array.from({ length: 8 }, () =>
                client.callTool<JsonObject & { operation_id: string }>(
                    "copy_path",
                    arguments_,
                ),
            ),
        );
        const operationId =
            responses[0]?.result.structuredContent?.operation_id;
        if (operationId === undefined)
            throw new Error("Transfer admission missing");
        // Concurrent retries must share one admitted operation, including while metadata is being fetched.
        expect(
            responses.map(
                (response) => response.result.structuredContent?.operation_id,
            ),
        ).toEqual(Array.from({ length: 8 }, () => operationId));
        // Copy and move have different side effects even when their arguments match exactly.
        await expect(client.callTool("move_path", arguments_)).rejects.toThrow(
            "different arguments",
        );
        // Completion must reach durable storage even if no client ever asks for operation progress.
        await expect
            .poll(() => {
                const metadata: { records: { id: string; state: string }[] } =
                    JSON.parse(readFileSync(options.metadataPath, "utf8"));
                return metadata.records.find(
                    (record) => record.id === operationId,
                )?.state;
            })
            .toBe("completed");
        // The admitted copy must still complete and publish the original bytes.
        expect(
            (await client.waitForOperation(operationId)).result
                .structuredContent?.state,
        ).toBe("completed");
        expect(readFileSync(destinationPath, "utf8")).toBe("one transfer only");
        const canceled = await client.callTool("cancel_operation", {
            operation_id: operationId,
        });
        // A terminal record cannot route cancellation to any live router work.
        expect(canceled.result.structuredContent).toMatchObject({
            cancellation_accepted: false,
            termination_acknowledged: true,
        });
    });

    it("shares a failed keyed admission without repeating remote work", async () => {
        const client = createMcpTestClient({
            endpoint: options.endpoint,
            token: options.token(),
        });
        const arguments_: JsonObject = {
            source: {
                device_id: options.sourceDevice,
                path: join(options.sourceRoot, "missing-retry-source"),
            },
            destination: {
                device_id: options.destinationDevice,
                path: join(options.destinationRoot, "failed-retry-destination"),
            },
            on_existing: "error",
            idempotency_key: "failed-concurrent-transfer",
        };
        const responses = await Promise.all(
            Array.from({ length: 4 }, () =>
                client.callTool<JsonObject & { operation_id: string }>(
                    "copy_path",
                    arguments_,
                ),
            ),
        );
        const operationId =
            responses[0]?.result.structuredContent?.operation_id;
        if (operationId === undefined)
            throw new Error("Failed admission missing");
        // Even failed remote starts belong to the same durable keyed admission.
        expect(
            responses.map(
                (response) => response.result.structuredContent?.operation_id,
            ),
        ).toEqual(Array.from({ length: 4 }, () => operationId));
        expect(
            (await client.getOperation(operationId)).result.structuredContent,
        ).toMatchObject({
            state: "failed",
            termination: { reason: "admission_failed" },
        });
    });
}
