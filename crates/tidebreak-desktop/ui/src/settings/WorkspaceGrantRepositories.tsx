import { useId, useState, type FormEvent } from "react";

import type { CodeGrantSnapshot } from "../api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { SettingsField } from "./primitives";

type ChannelRepository = NonNullable<CodeGrantSnapshot["channels"]>[number];
type ApproveRepositories = (
  channelId: string,
  repositories: string[],
) => Promise<boolean>;

export function WorkspaceGrantRepositories({
  grant,
  disabled,
  onApprove,
}: {
  grant: CodeGrantSnapshot;
  disabled: boolean;
  onApprove: ApproveRepositories;
}) {
  const [addingChannel, setAddingChannel] = useState(false);
  const channels = new Map<string, ChannelRepository[]>();
  for (const repository of grant.channels ?? []) {
    const entries = channels.get(repository.channel_id) ?? [];
    entries.push(repository);
    channels.set(repository.channel_id, entries);
  }
  const canApprove = !grant.revoked_at && grant.channel_kind === "slack";

  return (
    <div className="flex min-w-0 flex-col gap-4 border-t pt-3">
      <div className="flex flex-col gap-1">
        <h3 className="text-sm font-medium">Channel repositories</h3>
        {canApprove && (
          <p className="text-sm text-muted-foreground">
            Admins approve repositories for each channel once. Agents can choose
            any approved repository and work across them. The shared GitHub
            identity must also have access. To use newly approved repositories,
            ask the agent to retry in Slack.
          </p>
        )}
      </div>
      {channels.size === 0 ? (
        <p className="text-sm text-muted-foreground">
          No channel repositories yet. Approve repositories before the first
          task by adding a Slack channel ID.
        </p>
      ) : (
        <div className="flex flex-col divide-y">
          {[...channels.entries()].map(([channelId, repositories]) => (
            <ChannelRepositoryGroup
              key={channelId}
              channelId={channelId}
              repositories={repositories}
              canApprove={canApprove}
              disabled={disabled}
              onApprove={onApprove}
            />
          ))}
        </div>
      )}
      {canApprove &&
        (addingChannel ? (
          <RepositoryApprovalForm
            disabled={disabled}
            onApprove={onApprove}
            onClose={() => setAddingChannel(false)}
          />
        ) : (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            disabled={disabled}
            onClick={() => setAddingChannel(true)}
          >
            Add channel
          </Button>
        ))}
    </div>
  );
}

function ChannelRepositoryGroup({
  channelId,
  repositories,
  canApprove,
  disabled,
  onApprove,
}: {
  channelId: string;
  repositories: ChannelRepository[];
  canApprove: boolean;
  disabled: boolean;
  onApprove: ApproveRepositories;
}) {
  const [editing, setEditing] = useState(false);
  const headingId = useId();
  const approved = repositories.filter((entry) => entry.state === "confirmed");
  const pending = repositories.filter((entry) => entry.state === "pending");
  const pendingRepositories = [
    ...new Set(pending.map((entry) => entry.repository)),
  ];
  const previous = repositories.filter(
    (entry) => entry.state !== "confirmed" && entry.state !== "pending",
  );

  return (
    <section
      aria-labelledby={headingId}
      className="flex min-w-0 flex-col gap-3 py-3 first:pt-0 last:pb-0"
    >
      <h4 id={headingId} className="break-all font-mono text-sm font-medium">
        Channel {channelId}
      </h4>
      <div className="flex flex-col gap-1">
        <p className="text-xs text-muted-foreground">Approved repositories</p>
        {approved.length > 0 ? (
          <RepositoryList repositories={approved} />
        ) : (
          <p className="text-sm text-muted-foreground">None approved yet.</p>
        )}
      </div>
      {pending.length > 0 && (
        <div className="flex flex-col items-start gap-2">
          <div className="flex w-full min-w-0 flex-col gap-1">
            <p className="text-xs text-muted-foreground">Pending approval</p>
            <RepositoryList repositories={pending} />
          </div>
          {canApprove && (
            <Button
              type="button"
              variant="outline"
              size="sm"
              disabled={disabled}
              onClick={() =>
                void onApprove(channelId, pendingRepositories.slice(0, 100))
              }
            >
              {pendingRepositories.length > 100
                ? "Approve first 100"
                : "Approve all pending repositories"}
            </Button>
          )}
        </div>
      )}
      {previous.length > 0 && (
        <details className="text-sm text-muted-foreground">
          <summary className="cursor-pointer rounded-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
            Previous repositories ({previous.length})
          </summary>
          <ul className="mt-2 flex flex-col gap-1">
            {previous.map((entry) => (
              <li key={entry.repository} className="break-all">
                <span className="font-mono">{entry.repository}</span> ·{" "}
                {entry.state}
              </li>
            ))}
          </ul>
        </details>
      )}
      {canApprove &&
        (editing ? (
          <RepositoryApprovalForm
            channelId={channelId}
            disabled={disabled}
            onApprove={onApprove}
            onClose={() => setEditing(false)}
          />
        ) : (
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="self-start"
            disabled={disabled}
            onClick={() => setEditing(true)}
          >
            Add repositories
          </Button>
        ))}
    </section>
  );
}

function RepositoryList({
  repositories,
}: {
  repositories: ChannelRepository[];
}) {
  return (
    <ul className="flex flex-col gap-1">
      {repositories.map((entry) => (
        <li key={entry.repository} className="break-all font-mono text-sm">
          {entry.repository}
        </li>
      ))}
    </ul>
  );
}

function RepositoryApprovalForm({
  channelId,
  disabled,
  onApprove,
  onClose,
}: {
  channelId?: string;
  disabled: boolean;
  onApprove: ApproveRepositories;
  onClose: () => void;
}) {
  const [channel, setChannel] = useState(channelId ?? "");
  const [text, setText] = useState("");
  const [error, setError] = useState<string | null>(null);
  const errorId = useId();

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (disabled) return;
    const repositories = [
      ...new Set(
        text
          .split(/[\n,]+/)
          .map((item) => item.trim())
          .filter(Boolean),
      ),
    ];
    if (!channel.trim()) {
      setError("Enter the Slack channel ID.");
      return;
    }
    if (repositories.length === 0 || repositories.length > 100) {
      setError("Enter between 1 and 100 explicit repositories.");
      return;
    }
    if (repositories.some((repository) => repository.includes("*"))) {
      setError(
        "Wildcards are not supported. Enter each repository explicitly.",
      );
      return;
    }
    setError(null);
    if (await onApprove(channel.trim(), repositories)) onClose();
  }

  return (
    <form
      aria-label={
        channelId
          ? `Add repositories to ${channelId}`
          : "Add channel repositories"
      }
      className="flex min-w-0 flex-col gap-3"
      onSubmit={(event) => void submit(event)}
    >
      {!channelId && (
        <SettingsField
          label="Slack channel ID"
          hint="Open the channel details in Slack to copy its channel ID."
        >
          <Input
            value={channel}
            onChange={(event) => setChannel(event.target.value)}
            placeholder="C0123456789"
            disabled={disabled}
            autoComplete="off"
            spellCheck={false}
          />
        </SettingsField>
      )}
      <SettingsField
        label="Repositories"
        hint="Enter GitHub owner/repository names or URLs, one per line or separated by commas. Approvals add to this channel’s existing access. Wildcards are not supported. Up to 100 repositories per approval."
      >
        <Textarea
          value={text}
          onChange={(event) => setText(event.target.value)}
          placeholder={"acme/service\nhttps://github.com/acme/web"}
          rows={3}
          className="font-mono"
          disabled={disabled}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={error ? errorId : undefined}
          aria-invalid={Boolean(error)}
        />
      </SettingsField>
      {error && (
        <p
          id={errorId}
          className="notice-surface notice-critical rounded-md border px-3 py-2 text-sm"
          role="alert"
        >
          {error}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        <Button type="submit" size="sm" disabled={disabled}>
          Approve repositories
        </Button>
        <Button
          type="button"
          variant="outline"
          size="sm"
          disabled={disabled}
          onClick={onClose}
        >
          Cancel
        </Button>
      </div>
    </form>
  );
}
