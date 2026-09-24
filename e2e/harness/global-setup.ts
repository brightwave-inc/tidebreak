import { rendererBundle, serverBinary } from "./paths";
import { publishServices, startServices, stopServices } from "./services";

/**
 * Check the lane's inputs, then start PostgreSQL and the S3 gateway once for
 * the whole run. The returned function removes both containers.
 */
export default async function globalSetup(): Promise<() => Promise<void>> {
  // Resolved once and handed to the workers, which inherit this environment.
  // Missing inputs fail here, before any container starts.
  process.env.TIDEBREAK_E2E_BINARY = serverBinary();
  process.env.TIDEBREAK_E2E_UI_DIST = rendererBundle();
  const services = await startServices();
  publishServices(services);
  return async () => {
    await stopServices(services);
  };
}
