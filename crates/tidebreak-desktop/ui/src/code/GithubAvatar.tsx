import { useState } from "react";

import { cn } from "@/lib/utils";
import { githubAvatarUrl } from "./pullRequestPresentation";

/**
 * A GitHub login's face at list scale. Initials stand in when there is no
 * image, or when the image fails to load.
 */
export function GithubAvatar({
  login,
  url,
  className,
}: {
  login: string | undefined;
  url?: string;
  className?: string;
}) {
  const [failed, setFailed] = useState(false);
  const source = url ?? githubAvatarUrl(login);
  if (!source || failed) {
    return (
      <span
        className={cn(
          "grid size-5 shrink-0 place-items-center rounded-full bg-muted text-2xs font-semibold uppercase text-muted-foreground",
          className,
        )}
        aria-hidden
      >
        {(login ?? "?").slice(0, 2)}
      </span>
    );
  }
  return (
    <img
      src={source}
      alt=""
      className={cn("size-5 shrink-0 rounded-full object-cover", className)}
      onError={() => setFailed(true)}
    />
  );
}
