import { withSettingsApi } from "./client/settings";
import { withAppsApi } from "./client/apps";
import { withProjectsApi } from "./client/projects";
import { withChatApi } from "./client/chat";
import { withAgentRunsApi } from "./client/agent-runs";
import { withTurnsApi } from "./client/turns";
import { withDeliveryApi } from "./client/delivery";
import { withCodeReposApi } from "./client/code-repos";
import { withCodeWorkspacesApi } from "./client/code-workspaces";
import { withCodeSessionsApi } from "./client/code-sessions";
import { withCodeFilesApi } from "./client/code-files";
import { withCodeGitApi } from "./client/code-git";
import { withCodeGrantsApi } from "./client/code-grants";
import { withCodeTerminalsApi } from "./client/code-terminals";
import { withCodeEventsApi } from "./client/code-events";
import { HttpCore } from "./client/http";
export {
  ARCHIVE_FORCE_KINDS,
  HttpError,
  archiveForceKind,
  type DeliveryRequestOptions,
} from "./client/http";
export { type CodeWorkspaceMergeRequest } from "./client/code-git";

/**
 * The desktop's HTTP and WebSocket client, assembled from per-domain facets
 * over one HTTP core.
 *
 * Each module under `./client/` owns one server route family and adds its
 * methods through a mixin. This class only composes them, so call sites keep
 * `client.method()` and a change to one route family touches one file.
 */
export class ApiClient extends withCodeEventsApi(
  withCodeTerminalsApi(
    withCodeGrantsApi(
      withCodeGitApi(
        withCodeFilesApi(
          withCodeSessionsApi(
            withCodeWorkspacesApi(
              withCodeReposApi(
                withDeliveryApi(
                  withTurnsApi(
                    withAgentRunsApi(
                      withChatApi(
                        withProjectsApi(withAppsApi(withSettingsApi(HttpCore))),
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ),
        ),
      ),
    ),
  ),
) {
  constructor(baseUrl: string, token: string) {
    super(baseUrl, token);
  }
}
