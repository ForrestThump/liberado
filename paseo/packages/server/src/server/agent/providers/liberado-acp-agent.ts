import type { Logger } from "pino";

import type { AgentCapabilityFlags } from "../agent-sdk-types.js";
import type { ProviderRuntimeSettings } from "../provider-launch-config.js";
import { ACPAgentClient } from "./acp-agent.js";

const LIBERADO_ACP_CAPABILITIES: AgentCapabilityFlags = {
  supportsStreaming: true,
  supportsSessionPersistence: false,
  supportsDynamicModes: false,
  supportsMcpServers: false,
  supportsReasoningStream: false,
  supportsToolInvocations: false,
};

function resolveLiberadoAcpBinary(runtimeSettings?: ProviderRuntimeSettings): [string, ...string[]] {
  if (
    runtimeSettings?.command?.mode === "replace" &&
    runtimeSettings.command.argv.length > 0
  ) {
    return runtimeSettings.command.argv as [string, ...string[]];
  }
  return ["liberado-acp"];
}

export class LiberadoACPAgentClient extends ACPAgentClient {
  constructor(logger: Logger, runtimeSettings?: ProviderRuntimeSettings) {
    super({
      provider: "liberado-acp",
      logger,
      runtimeSettings,
      defaultCommand: resolveLiberadoAcpBinary(runtimeSettings),
      defaultModes: [],
      capabilities: LIBERADO_ACP_CAPABILITIES,
    });
  }
}
