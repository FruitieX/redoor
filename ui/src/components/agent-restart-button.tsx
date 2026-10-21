import type { Agent } from "#ui/api-client";
import { RestartButton } from "#ui/components/restart-button";
import { useAgentReconnect } from "#ui/use-agent-reconnect";

/**
 * The confirmed restart for one agent, shared by the device list and the device page so the
 * wording and the wait for the agent to reconnect cannot differ between them.
 */
export function AgentRestartButton(props: { agent: Agent }) {
    const { waitForAgentRestart } = useAgentReconnect(props.agent);

    return (
        <RestartButton
            target={`agent ${props.agent.name}`}
            description="The agent will restart with the same arguments. In-flight transfers and terminals are interrupted."
            restart={() => props.agent.restart()}
            waitUntilReady={waitForAgentRestart}
        />
    );
}
