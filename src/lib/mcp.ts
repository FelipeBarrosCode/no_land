import { invokeSafe } from "./tauri";
export { buildMcpSetupPrompt, MCP_SETUP_PROMPT } from "./mcpSetup";

export interface McpConfig {
  enabled: boolean; port: number; instances: number[]; allInstances: boolean; control: boolean;
  shell: boolean; elevated: boolean; desktop: boolean; dialogs: boolean; localRoots: string[];
}
export interface McpApproval {
  id: string; tool: string; instanceId: number | null; arguments: unknown;
  status: string; createdAt: string;
}
export interface McpStatus {
  config: McpConfig; endpoint: string; listening: boolean; error: string | null;
  fingerprint: string | null; activeJobs: number; clients: { id: string; name: string; connectedAt: string }[];
  approvals: McpApproval[];
  recentActivity: { sequence: number; at: string; kind: string; details: Record<string, unknown> }[];
}
export const mcpStatus = () => invokeSafe<McpStatus>("mcp_status");
export const mcpConfigure = (config: McpConfig) => invokeSafe<McpStatus>("mcp_configure", { config });
export const mcpDisable = () => invokeSafe<McpStatus>("mcp_disable");
export const mcpAccessToken = () => invokeSafe<string>("mcp_access_token");
export const mcpRotateToken = () => invokeSafe<McpStatus>("mcp_rotate_token");
export const mcpRevokeToken = () => invokeSafe<McpStatus>("mcp_revoke_token");
export const mcpDisconnectAgents = () => invokeSafe<McpStatus>("mcp_disconnect_agents");
export const mcpDecide = (id: string, approve: boolean) => invokeSafe<void>("mcp_decide_approval", { id, approve });
