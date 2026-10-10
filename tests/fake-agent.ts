import WebSocket from "ws";
import { z } from "zod";

import type { BinaryIdentity } from "#bindings/BinaryIdentity";
import { TEST_AGENT_TOKEN, webSocketDataToString } from "./test-utils";

/** Subprotocol of the agent websocket that this helper speaks. */
export const AGENT_PROTOCOL = "redoor.v2";

/** Path of the single agent websocket endpoint. */
export const AGENT_WS_PATH = "/api/v1/agent/ws";

/** Path of the payload socket that carries binary stream frames. */
const AGENT_TRANSFER_WS_PATH = "/api/v1/agent-transfer/ws";

/** Fixed header length of one binary stream frame. */
const STREAM_HEADER_SIZE = 23;

/** Leading marker of binary stream frames. */
const STREAM_MAGIC = 0x52415844;

/** Every control frame is a JSON object tagged by `type`. */
const controlFrameSchema = z
    .object({ type: z.string() })
    .catchall(z.unknown());

/** One parsed control frame received from the server. */
export type ControlFrame = z.infer<typeof controlFrameSchema>;

/** A `command` frame: the server asks the agent to run one request. */
const commandFrameSchema = z.object({
    type: z.literal("command"),
    id: z.number(),
    command: z.object({ type: z.string() }).catchall(z.unknown()),
});

/** One command the fake agent received. */
export type FakeCommand = z.infer<typeof commandFrameSchema>;

/** The server's answer to `hello` when it accepted the agent. */
const welcomeSchema = z.object({
    type: z.literal("welcome"),
    connection_id: z.string(),
});

/** The server's answer to `hello` when it refused the agent. */
const rejectedSchema = z.object({
    type: z.literal("rejected"),
    reason: z.string(),
    message: z.string(),
});

/** The bootstrap that tells the agent how to open its payload socket. */
const transferSocketOpenSchema = z.object({
    type: z.literal("transfer_socket_open"),
    token: z.string().min(1),
});

/** Upgrade request settings; every field defaults to what a correct agent sends. */
export type UpgradeOptions = {
    serverPort: number;
    /** Endpoint path; the agent endpoint unless a test probes another one. */
    path?: string;
    /** Bearer token; `null` sends no `Authorization` header at all. */
    token?: string | null;
    /** Offered subprotocols; an empty list sends no `Sec-WebSocket-Protocol` header. */
    protocols?: string[];
    /** Value of the agent version header used in rejection diagnostics. */
    version?: string;
    /** Whether `ws` answers pings itself; stale-detection tests turn this off. */
    autoPong?: boolean;
};

/** What the server answered to an upgrade attempt. */
export type UpgradeOutcome =
    | { kind: "open"; socket: WebSocket }
    | { kind: "refused"; status: number; body: string };

/**
 * Attempts one agent websocket upgrade and reports either the open socket or the HTTP answer
 * that refused it, so tests can assert on `401` and `426` without a socket ever existing.
 */
export async function upgradeAgentSocket(
    options: UpgradeOptions,
): Promise<UpgradeOutcome> {
    const url = `ws://127.0.0.1:${options.serverPort}${options.path ?? AGENT_WS_PATH}`;
    const token = options.token === undefined ? TEST_AGENT_TOKEN : options.token;

    const versionHeader = {
        "X-Redoor-Agent-Version": options.version ?? "0.0.0-fake-agent",
    };

    const headers =
        token === null
            ? versionHeader
            : { ...versionHeader, Authorization: `Bearer ${token}` };

    const socket = new WebSocket(url, options.protocols ?? [AGENT_PROTOCOL], {
        headers,
        autoPong: options.autoPong ?? true,
    });

    return new Promise<UpgradeOutcome>((resolve, reject) => {
        socket.once("open", () => resolve({ kind: "open", socket }));
        socket.once("unexpected-response", (_request, response) => {
            const chunks: Buffer[] = [];
            response.on("data", (chunk: Buffer) => chunks.push(chunk));
            response.on("end", () =>
                resolve({
                    kind: "refused",
                    status: response.statusCode ?? 0,
                    body: Buffer.concat(chunks).toString(),
                }),
            );
        });
        socket.once("error", reject);
    });
}

/** Identity and capabilities a fake agent announces in its `hello`. */
export type HelloOptions = {
    agentId: string;
    /** Defaults to the agent id, like real agents. */
    agentName?: string;
    os?: string;
    arch?: string;
    hostname?: string;
    username?: string;
    cwd?: string;
    binary?: Partial<BinaryIdentity>;
    supportsSelfExec?: boolean;
    supportsNativeOpen?: boolean;
    supportsTrash?: boolean;
};

/** The flat `hello` frame a real agent sends first on every connection. */
export type HelloFrame = {
    type: "hello";
    agent_id: string;
    agent_name: string;
    os: string;
    arch: string;
    hostname: string;
    username: string;
    cwd: string;
    binary: BinaryIdentity;
    supports_self_exec: boolean;
    supports_native_open: boolean;
    supports_trash: boolean;
};

/** Builds the `hello` frame for one fake agent identity. */
export function helloFrame(options: HelloOptions): HelloFrame {
    return {
        type: "hello",
        agent_id: options.agentId,
        agent_name: options.agentName ?? options.agentId,
        os: options.os ?? "linux",
        arch: options.arch ?? "x86_64",
        hostname: options.hostname ?? "fake-host",
        username: options.username ?? "fake-user",
        cwd: options.cwd ?? "/tmp",
        binary: {
            version: "0.0.0",
            git_rev: "fake",
            git_dirty: false,
            version_dirty: false,
            build_mode: "debug",
            build_date: "unknown",
            ...options.binary,
        } satisfies BinaryIdentity,
        supports_self_exec: options.supportsSelfExec ?? false,
        supports_native_open: options.supportsNativeOpen ?? false,
        supports_trash: options.supportsTrash ?? false,
    };
}

/** Thrown when the server answers `hello` with `rejected`. */
export class FakeAgentRejectedError extends Error {
    constructor(
        readonly reason: string,
        message: string,
    ) {
        super(message);
    }
}

/** Settings for a fully connected fake agent. */
export type FakeAgentOptions = HelloOptions &
    Omit<UpgradeOptions, "path" | "protocols"> & {
        /** Whether to open the payload socket the server asks for; on by default. */
        openTransfer?: boolean;
    };

/** One queued control frame consumer. */
type FrameWaiter = {
    accepts: (frame: ControlFrame) => boolean;
    resolve: (frame: ControlFrame) => void;
};

/**
 * A scripted agent speaking the real wire protocol over real sockets.
 *
 * Received control frames are queued so tests can wait for them in any order without racing
 * the socket; command handlers see every command, including ones that arrived earlier.
 */
export class FakeAgent {
    private readonly queued: ControlFrame[] = [];
    private readonly waiters: FrameWaiter[] = [];
    private readonly commandHandlers: ((command: FakeCommand) => void)[] = [];
    private readonly receivedCommands: FakeCommand[] = [];
    /** Payload socket, once the server asked for it and it was opened. */
    transfer: WebSocket | undefined;
    /** Connection id from the server's `welcome`. */
    connectionId = "";

    private constructor(
        readonly agentId: string,
        readonly control: WebSocket,
    ) {
        control.on("message", (data, isBinary) => {
            if (isBinary) {
                return;
            }

            this.receive(
                controlFrameSchema.parse(
                    JSON.parse(webSocketDataToString(data)),
                ),
            );
        });
    }

    /**
     * Upgrades, says hello and waits for the welcome; then opens the payload socket unless
     * told not to. Throws [`FakeAgentRejectedError`] when the server refuses the hello.
     */
    static async connect(options: FakeAgentOptions): Promise<FakeAgent> {
        const outcome = await upgradeAgentSocket(options);

        if (outcome.kind === "refused") {
            throw new Error(
                `agent upgrade refused with ${outcome.status}: ${outcome.body}`,
            );
        }

        const agent = new FakeAgent(options.agentId, outcome.socket);
        agent.send(helloFrame(options));

        const answer = await agent.waitForFrame(
            (frame) => frame.type === "welcome" || frame.type === "rejected",
        );

        const rejected = rejectedSchema.safeParse(answer);

        if (rejected.success) {
            agent.close();
            throw new FakeAgentRejectedError(
                rejected.data.reason,
                rejected.data.message,
            );
        }

        agent.connectionId = welcomeSchema.parse(answer).connection_id;

        if (options.openTransfer ?? true) {
            await agent.openTransfer(options.serverPort);
        }

        return agent;
    }

    /** Opens the payload socket with the token from `transfer_socket_open`. */
    private async openTransfer(serverPort: number): Promise<void> {
        const bootstrap = transferSocketOpenSchema.parse(
            await this.waitForFrame(
                (frame) => frame.type === "transfer_socket_open",
            ),
        );

        const outcome = await upgradeAgentSocket({
            serverPort,
            path: AGENT_TRANSFER_WS_PATH,
            token: bootstrap.token,
        });

        if (outcome.kind === "refused") {
            throw new Error(
                `transfer upgrade refused with ${outcome.status}: ${outcome.body}`,
            );
        }

        this.transfer = outcome.socket;
    }

    /** Hands a received frame to the oldest waiter that wants it, or queues it. */
    private receive(frame: ControlFrame): void {
        const command = commandFrameSchema.safeParse(frame);

        if (command.success) {
            this.receivedCommands.push(command.data);

            for (const handler of this.commandHandlers) {
                handler(command.data);
            }
        }

        const waiter = this.waiters.findIndex((entry) => entry.accepts(frame));

        if (waiter >= 0) {
            const [entry] = this.waiters.splice(waiter, 1);
            entry?.resolve(frame);

            return;
        }

        this.queued.push(frame);
    }

    /** Resolves with the first queued or future control frame `accepts` matches. */
    waitForFrame(
        accepts: (frame: ControlFrame) => boolean,
        timeoutMs = 10000,
    ): Promise<ControlFrame> {
        const index = this.queued.findIndex(accepts);

        if (index >= 0) {
            const [frame] = this.queued.splice(index, 1);

            if (frame !== undefined) {
                return Promise.resolve(frame);
            }
        }

        return new Promise((resolve, reject) => {
            const waiter: FrameWaiter = {
                accepts,
                resolve: (frame) => {
                    clearTimeout(timeout);
                    resolve(frame);
                },
            };

            const timeout = setTimeout(() => {
                const position = this.waiters.indexOf(waiter);

                if (position >= 0) {
                    this.waiters.splice(position, 1);
                }

                reject(new Error("Timed out waiting for a control frame"));
            }, timeoutMs);

            this.waiters.push(waiter);
        });
    }

    /** Waits for the next command of one type. */
    async nextCommand(type: string, timeoutMs = 10000): Promise<FakeCommand> {
        return commandFrameSchema.parse(
            await this.waitForFrame((frame) => {
                const command = commandFrameSchema.safeParse(frame);

                return command.success && command.data.command.type === type;
            }, timeoutMs),
        );
    }

    /** Calls `handler` for every command, starting with those already received. */
    onCommand(handler: (command: FakeCommand) => void): void {
        this.commandHandlers.push(handler);

        for (const command of this.receivedCommands) {
            handler(command);
        }
    }

    /** Commands received so far, in arrival order. */
    get commands(): FakeCommand[] {
        return [...this.receivedCommands];
    }

    /** Sends one control frame, tagged by `type` like every frame on the wire. */
    send<Frame extends { type: string }>(frame: Frame): void {
        this.control.send(JSON.stringify(frame));
    }

    /** Answers request `id` with a command result in its wire shape. */
    respond<Result extends { type: string }>(id: number, result: Result): void {
        this.send({ type: "command_response", id, result });
    }

    /** Sends one binary stream frame of request `id` on the payload socket. */
    sendStreamFrame(
        id: number,
        data: Buffer,
        flags: { last?: boolean; error?: boolean; tar?: boolean } = {},
    ): void {
        if (this.transfer === undefined) {
            throw new Error("the payload socket is not open");
        }

        this.transfer.send(encodeStreamFrame(id, data, flags));
    }

    /** Closes both sockets. */
    close(): void {
        this.control.close();
        this.transfer?.close();
    }
}

/** Encodes one binary stream frame the way the agent's stream writer does. */
export function encodeStreamFrame(
    id: number,
    data: Buffer,
    flags: { last?: boolean; error?: boolean; tar?: boolean } = {},
): Buffer {
    const header = Buffer.alloc(STREAM_HEADER_SIZE);
    header.writeUInt32LE(STREAM_MAGIC, 0);
    header.writeBigUInt64LE(BigInt(id), 4);
    header.writeBigUInt64LE(0n, 12);
    header.writeUInt8(flags.last ? 1 : 0, 20);
    header.writeUInt8(flags.error ? 1 : 0, 21);
    header.writeUInt8(flags.tar ? 1 : 0, 22);

    return Buffer.concat([header, data]);
}
