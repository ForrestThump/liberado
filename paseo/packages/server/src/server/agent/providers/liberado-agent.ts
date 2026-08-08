import type { ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import net from "node:net";
import type { Logger } from "pino";

import type {
  AgentClient,
  AgentCreateSessionOptions,
  AgentLaunchContext,
  AgentMode,
  AgentRunOptions,
  AgentRunResult,
  AgentSession,
  AgentSessionConfig,
  AgentStreamEvent,
  AgentRuntimeInfo,
  AgentPermissionRequest,
  AgentPermissionResponse,
  AgentPersistenceHandle,
  FetchCatalogOptions,
  ProviderCatalog,
} from "../agent-sdk-types.js";
import {
  createProviderEnvSpec,
  type ProviderRuntimeSettings,
} from "../provider-launch-config.js";
import { spawnProcess, type SpawnProcessOptions } from "../../../utils/spawn.js";
import { terminateWithTreeKill } from "../../../utils/tree-kill.js";
import { findExecutable } from "../../../executable-resolution/executable-resolution.js";

async function findAvailablePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server.address();
      if (addr && typeof addr === "object") {
        const { port } = addr;
        server.close(() => resolve(port));
      } else {
        reject(new Error("could not get port from server"));
      }
    });
  });
}

function resolveLiberadoBinary(): Promise<string> {
  const explicit = process.env.LIBERADO_BINARY;
  if (explicit) return Promise.resolve(explicit);
  return findExecutable("liberado").then((found) => {
    if (!found) throw new Error("liberado binary not found on PATH. Set LIBERADO_BINARY env var.");
    return found;
  });
}

interface LiberadoServer {
  process: ChildProcess;
  port: number;
  baseUrl: string;
}

interface LiberadoSseEvent {
  event?: string;
  data?: string;
}

async function waitForServer(baseUrl: string, timeoutMs = 15_000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const resp = await fetch(`${baseUrl}/api/status`);
      if (resp.ok) return;
    } catch {
      // not ready yet
    }
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error(`liberado server did not become ready within ${timeoutMs}ms`);
}

async function startLiberadoServer(
  logger: Logger,
  runtimeSettings?: ProviderRuntimeSettings,
  cwdOverride?: string,
): Promise<LiberadoServer> {
  const binary = await resolveLiberadoBinary();
  const port = await findAvailablePort();
  const baseUrl = `http://127.0.0.1:${port}`;

  logger.info({ binary, port }, "starting liberado server");

  const vaultPath = cwdOverride ?? process.cwd();

  const envSpec = createProviderEnvSpec({
    runtimeSettings,
    overlays: [{ LIBERADO_PORT: String(port), LIBERADO_VAULT: vaultPath }],
  });

  const child = spawnProcess(binary, ["serve"], {
    cwd: vaultPath,
    detached: process.platform !== "win32",
    stdio: ["ignore", "pipe", "pipe"],
    env: { ...envSpec.baseEnv, ...envSpec.envOverlay },
    shell: false,
  } as SpawnProcessOptions);

  child.stdout?.on("data", (d: Buffer) => logger.debug({ stdout: d.toString() }, "liberado stdout"));
  child.stderr?.on("data", (d: Buffer) => logger.debug({ stderr: d.toString() }, "liberado stderr"));
  child.on("exit", (code: number | null) => logger.info({ code }, "liberado server exited"));
  child.on("error", (err: Error) => logger.error({ err }, "liberado server error"));

  await waitForServer(baseUrl);
  logger.info({ baseUrl }, "liberado server ready");

  return { process: child, port, baseUrl };
}

async function stopLiberadoServer(server: LiberadoServer, logger: Logger): Promise<void> {
  logger.info("stopping liberado server");
  await terminateWithTreeKill(server.process, {
    gracefulTimeoutMs: 5_000,
    forceTimeoutMs: 1_000,
  });
}

function parseSseStream(body: ReadableStream<Uint8Array>): AsyncGenerator<LiberadoSseEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";

  return (async function* () {
    try {
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split("\n");
        buffer = lines.pop() ?? "";

        let event: LiberadoSseEvent = {};
        for (const line of lines) {
          if (line.startsWith("event:")) {
            event.event = line.slice(6).trim();
          } else if (line.startsWith("data:")) {
            event.data = line.slice(5).trim();
          } else if (line === "") {
            if (event.event || event.data) {
              yield event;
              event = {};
            }
          }
        }
      }
    } finally {
      reader.releaseLock();
    }
  })();
}

// ── Session ────────────────────────────────────────────────────────────────

class LiberadoSession implements AgentSession {
  readonly provider = "liberado" as const;
  readonly id: string | null;
  readonly capabilities = {
    supportsStreaming: true,
    supportsSessionPersistence: true,
    supportsDynamicModes: false,
    supportsMcpServers: false,
    supportsReasoningStream: false,
    supportsToolInvocations: true,
  };

  private readonly logger: Logger;
  private readonly baseUrl: string;
  private server: LiberadoServer;
  private conversationId: string | null = null;
  private abortController: AbortController | null = null;
  private subscribers = new Set<(event: AgentStreamEvent) => void>();
  private closed = false;

  constructor(
    logger: Logger,
    baseUrl: string,
    server: LiberadoServer,
    sessionId?: string,
  ) {
    this.logger = logger;
    this.baseUrl = baseUrl;
    this.server = server;
    this.id = sessionId ?? null;
  }

  async run(
    _prompt: unknown,
    _options?: AgentRunOptions,
  ): Promise<AgentRunResult> {
    throw new Error("Liberado does not support synchronous run(); use startTurn() + subscribe()");
  }

  async startTurn(
    prompt: unknown,
    _options?: AgentRunOptions,
  ): Promise<{ turnId: string }> {
    if (this.closed) throw new Error("session closed");
    const turnId = randomUUID();

    const pc = new AbortController();
    this.abortController = pc;

    this.notify({ type: "turn_started", provider: "liberado", turnId });

    const promptText = typeof prompt === "string"
      ? prompt
      : (prompt as Record<string, unknown>)?.text as string ?? JSON.stringify(prompt);

    void this.streamChat(promptText, turnId, pc.signal);
    return { turnId };
  }

  private async streamChat(prompt: string, turnId: string, signal: AbortSignal): Promise<void> {
    try {
      const streamResp = await fetch(`${this.baseUrl}/api/chat/stream`, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          ...(this.conversationId
            ? { "X-Liberado-Session-Id": this.conversationId }
            : {}),
        },
        body: JSON.stringify({ message: prompt }),
        signal,
      });

      if (!streamResp.ok) {
        this.notify({
          type: "turn_failed",
          provider: "liberado",
          error: `Liberado returned ${streamResp.status}`,
          turnId,
        });
        return;
      }

      if (!this.conversationId) {
        const sid = streamResp.headers.get("x-session-id")
          ?? streamResp.headers.get("x-liberado-session-id");
        if (sid) {
          this.conversationId = sid;
          this.notify({
            type: "thread_started",
            sessionId: sid,
            provider: "liberado",
          });
        }
      }

      if (!streamResp.body) {
        this.notify({ type: "turn_failed", provider: "liberado", error: "no response body", turnId });
        return;
      }

      for await (const sse of parseSseStream(streamResp.body)) {
        if (signal.aborted) break;
        this.handleSseEvent(sse, turnId);
      }
    } catch (err: unknown) {
      if ((err as Error).name === "AbortError") return;
      this.logger.error({ err }, "liberado stream error");
      this.notify({
        type: "turn_failed",
        provider: "liberado",
        error: (err as Error).message,
        turnId,
      });
    }
  }

  private handleSseEvent(sse: LiberadoSseEvent, turnId: string): void {
    const eventType = sse.event ?? "";
    const data = sse.data ?? "";

    switch (eventType) {
      case "token":
        this.notify({
          type: "timeline",
          item: { type: "assistant_message", text: data },
          provider: "liberado",
          turnId,
        });
        break;

      case "tool_started": {
        let name = "", argsPreview = "";
        try { const p = JSON.parse(data); name = p.name ?? ""; argsPreview = p.args_preview ?? ""; } catch { /* */ }
        this.notify({
          type: "timeline",
          item: {
            type: "tool_call",
            callId: randomUUID(),
            name,
            status: "running",
            error: null,
            detail: { type: "shell", command: argsPreview },
          },
          provider: "liberado",
          turnId,
        });
        break;
      }

      case "tool_finished": {
        let name = "", ok = true, resultPreview = "";
        try {
          const p = JSON.parse(data);
          name = p.name ?? "";
          ok = p.ok ?? true;
          resultPreview = p.result_preview ?? "";
        } catch { /* */ }
        this.notify({
          type: "timeline",
          item: {
            type: "tool_call",
            callId: randomUUID(),
            name,
            status: ok ? "completed" : "failed",
            error: null,
            detail: { type: "shell", command: resultPreview },
          },
          provider: "liberado",
          turnId,
        });
        break;
      }

      case "session_finished":
        this.notify({ type: "turn_completed", provider: "liberado", turnId });
        break;

      case "failed": {
        let msg = data;
        try { msg = JSON.parse(data).message ?? data; } catch { /* */ }
        this.notify({ type: "turn_failed", provider: "liberado", error: msg, turnId });
        break;
      }

      case "session": {
        const sid = data;
        if (sid && !this.conversationId) {
          this.conversationId = sid;
          this.notify({ type: "thread_started", sessionId: sid, provider: "liberado" });
        }
        break;
      }

      default:
        break;
    }
  }

  subscribe(callback: (event: AgentStreamEvent) => void): () => void {
    this.subscribers.add(callback);
    return () => { this.subscribers.delete(callback); };
  }

  private notify(event: AgentStreamEvent): void {
    for (const sub of this.subscribers) {
      try { sub(event); } catch { /* swallow subscriber errors */ }
    }
  }

  async *streamHistory(): AsyncGenerator<AgentStreamEvent> {
    // Liberado sessions don't support history replay through this API yet
  }

  async getRuntimeInfo(): Promise<AgentRuntimeInfo> {
    return { provider: "liberado", sessionId: this.conversationId };
  }

  async getAvailableModes(): Promise<AgentMode[]> {
    return [];
  }

  async getCurrentMode(): Promise<string | null> {
    return null;
  }

  async setMode(): Promise<void> {
    // no-op: Liberado doesn't support modes yet
  }

  getPendingPermissions(): AgentPermissionRequest[] {
    return [];
  }

  async respondToPermission(
    _requestId: string,
    _response: AgentPermissionResponse,
  ): Promise<void> {
    // Liberado handles permissions internally
  }

  describePersistence(): AgentPersistenceHandle | null {
    return null;
  }

  async interrupt(): Promise<void> {
    this.abortController?.abort();
    if (this.conversationId) {
      try {
        await fetch(`${this.baseUrl}/api/conversations/${this.conversationId}/cancel`, {
          method: "POST",
        });
      } catch {
        // best effort
      }
    }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.abortController?.abort();
    this.subscribers.clear();
    if (this.server) {
      await stopLiberadoServer(this.server, this.logger);
    }
  }
}

// ── Agent Client ────────────────────────────────────────────────────────────

export class LiberadoAgentClient implements AgentClient {
  readonly provider = "liberado" as const;
  readonly capabilities = {
    supportsStreaming: true,
    supportsSessionPersistence: true,
    supportsDynamicModes: false,
    supportsMcpServers: false,
    supportsReasoningStream: false,
    supportsToolInvocations: true,
  };

  private readonly logger: Logger;
  private readonly runtimeSettings?: ProviderRuntimeSettings;

  constructor(logger: Logger, runtimeSettings?: ProviderRuntimeSettings) {
    this.logger = logger;
    this.runtimeSettings = runtimeSettings;
  }

  async createSession(
    config: AgentSessionConfig,
    _launchContext?: AgentLaunchContext,
    _options?: AgentCreateSessionOptions,
  ): Promise<AgentSession> {
    const server = await startLiberadoServer(this.logger, this.runtimeSettings, config.cwd);
    return new LiberadoSession(this.logger, server.baseUrl, server);
  }

  async resumeSession(
    handle: AgentPersistenceHandle,
    _overrides?: Partial<AgentSessionConfig>,
    _launchContext?: AgentLaunchContext,
    _options?: unknown,
  ): Promise<AgentSession> {
    const h = handle as { metadata?: { conversationId?: string } };
    const convId = h?.metadata?.conversationId;
    if (!convId) throw new Error("cannot resume: missing conversationId in handle metadata");
    const server = await startLiberadoServer(this.logger, this.runtimeSettings);
    return new LiberadoSession(this.logger, server.baseUrl, server, convId);
  }

  async fetchCatalog(_options: FetchCatalogOptions): Promise<ProviderCatalog> {
    return { models: [], modes: [] };
  }

  async isAvailable(): Promise<boolean> {
    try {
      await resolveLiberadoBinary();
      return true;
    } catch {
      return false;
    }
  }
}
