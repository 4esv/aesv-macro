// FIX: the self-hosted stack serves no /mcp route yet; W-2065 adds one.
export const MACRO_MCP_URL = `${globalThis.location?.origin ?? ''}/mcp`;

export const MACRO_MCP_CONFIG = JSON.stringify(
  {
    mcpServers: {
      aesv: {
        type: 'http',
        url: MACRO_MCP_URL,
      },
    },
  },
  null,
  2
);

export const CLI_COMMANDS = [
  {
    key: 'claude-cli',
    label: 'Claude Code',
    command: `claude mcp add --transport http aesv ${MACRO_MCP_URL}`,
  },
  {
    key: 'codex-cli',
    label: 'Codex CLI',
    command: `codex mcp add aesv --url ${MACRO_MCP_URL}`,
  },
] as const;

export const WEB_CLIENTS = [
  {
    key: 'claude-web',
    label: 'Claude.ai',
    hint: 'Settings → Connectors → Add custom connector',
  },
  {
    key: 'chatgpt-web',
    label: 'ChatGPT',
    hint: 'Settings → Apps → Advanced settings → enable Developer mode, then Create App',
  },
] as const;
