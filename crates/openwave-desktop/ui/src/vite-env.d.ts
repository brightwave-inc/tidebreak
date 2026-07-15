/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_OPENWAVE_URL?: string;
  readonly VITE_OPENWAVE_TOKEN?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
