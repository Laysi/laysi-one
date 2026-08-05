/// <reference types="vite/client" />

/** Vite environment variables this project uses, all declared explicitly to
 *  avoid any. */
interface ImportMetaEnv {
  /** `'1'` switches to the fixtures in `src/api/fixtures.ts` so the backend
   *  can be absent. Off by default. */
  readonly VITE_USE_FIXTURES?: string;
  /** REST base path, empty by default (same-origin `/v1`). */
  readonly VITE_API_BASE?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
