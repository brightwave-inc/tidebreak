/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_TIDEBREAK_URL?: string;
  readonly VITE_TIDEBREAK_TOKEN?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
