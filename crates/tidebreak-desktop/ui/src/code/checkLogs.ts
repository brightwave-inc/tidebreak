import { toast } from "sonner";

import type { ApiClient } from "../api/client";
import type { CodeCheckLog } from "../api/types";
import { friendlyErrorMessage } from "@/lib/utils";

/**
 * Failing CI logs, fetched before the fix-errors prompt goes out.
 *
 * The server downloads each failing GitHub Actions job's log into private
 * storage and hands back the paths, so the agent's first move is reading the
 * failure rather than working out which job produced it.
 *
 * Every failure here is soft. A GitHub outage, a signed-out `gh`, or a check
 * from a provider with no downloadable log all leave the reader with the
 * prompt they pressed for — just without the files. Refusing to send the turn
 * would be a worse trade than sending it the way it worked before.
 */
export async function fetchFixErrorsLogs(
  client: Pick<ApiClient, "writeCodeCheckLogs">,
  workspaceId: string,
): Promise<readonly CodeCheckLog[]> {
  try {
    const snapshot = await client.writeCodeCheckLogs(workspaceId);
    if (snapshot.errors.length > 0) {
      toast.message(
        snapshot.logs.length > 0
          ? `Could not download ${snapshot.errors.length} of the failing job logs`
          : "Could not download the failing job logs",
        { description: snapshot.errors[0]?.message },
      );
    }
    return snapshot.logs;
  } catch (err) {
    toast.message("Could not download the failing job logs", {
      description: friendlyErrorMessage(
        err,
        "The agent will read them itself.",
      ),
    });
    return [];
  }
}
