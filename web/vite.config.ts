import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

/** Backend address. Defaults to a local laysi-server during development. */
const API_TARGET = process.env.LAYSI_API_TARGET ?? 'http://localhost:8080';
/**
 * MCP address, defaulting to the same host as the API: a local `cargo run`
 * is LAYSI_SERVE=full and one process serves everything. Set this only to
 * reproduce the compose split topology locally, with separate processes in
 * app mode and mcp mode.
 */
const MCP_TARGET = process.env.LAYSI_MCP_TARGET ?? API_TARGET;

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    // /v1 (REST), /mcp (MCP streamable HTTP) and /healthz are all proxied to
    // the backend. /healthz is the only place the security screen can read
    // the RustFS state.
    proxy: {
      '/v1': { target: API_TARGET, changeOrigin: true },
      '/mcp': { target: MCP_TARGET, changeOrigin: true },
      '/healthz': { target: API_TARGET, changeOrigin: true },
    },
  },
  build: { outDir: 'dist', sourcemap: false },
});
