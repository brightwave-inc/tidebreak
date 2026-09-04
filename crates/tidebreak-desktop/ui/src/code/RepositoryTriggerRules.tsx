import { useEffect, useRef, useState } from "react";
import { CircleAlert, LoaderCircle } from "lucide-react";

import type { ApiClient } from "@/api/client";
import type {
  CodeGitHubRepositoryRef,
  CodeTriggerAction,
  CodeTriggerCondition,
  CodeTriggerSnapshot,
} from "@/api/types";
import { Button } from "@/components/ui/button";
import { friendlyErrorMessage } from "@/lib/utils";
import { CodeTriggerRules, type CodeTriggerTarget } from "./CodeTriggerRules";

type TriggerClient = Pick<
  ApiClient,
  | "listCodeTriggers"
  | "createCodeTrigger"
  | "setCodeTriggerEnabled"
  | "deleteCodeTrigger"
>;

export function RepositoryTriggerRules({
  client,
  repository,
  target = null,
}: {
  client: TriggerClient;
  repository: CodeGitHubRepositoryRef;
  target?: CodeTriggerTarget | null;
}) {
  const repoId = repository.tidebreak_repo_id;
  const [triggers, setTriggers] = useState<CodeTriggerSnapshot[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const busyRef = useRef(false);

  const load = async () => {
    const token = ++generation.current;
    if (!repoId) {
      setTriggers([]);
      setLoading(false);
      setError("Register this repository in Tidebreak before adding triggers.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const next = await client.listCodeTriggers(repoId);
      if (token === generation.current) setTriggers(next);
    } catch (caught) {
      if (token === generation.current) {
        setError(friendlyErrorMessage(caught, "Could not load triggers."));
      }
    } finally {
      if (token === generation.current) setLoading(false);
    }
  };

  useEffect(() => {
    busyRef.current = false;
    setBusy(false);
    setTriggers([]);
    void load();
    return () => {
      generation.current += 1;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, repoId]);

  const mutate = async <T,>(
    operation: () => Promise<T>,
    apply: (result: T) => void,
  ) => {
    if (busyRef.current || !repoId) return;
    const token = generation.current;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    try {
      const result = await operation();
      if (token === generation.current) apply(result);
    } catch (caught) {
      if (token === generation.current) {
        setError(friendlyErrorMessage(caught, "Could not update the trigger."));
      }
    } finally {
      if (token === generation.current) {
        busyRef.current = false;
        setBusy(false);
      }
    }
  };

  const upsert = (trigger: CodeTriggerSnapshot) => {
    setTriggers((current) => [
      ...current.filter((item) => item.id !== trigger.id),
      trigger,
    ]);
  };

  const arm = (condition: CodeTriggerCondition, action: CodeTriggerAction) => {
    if (!repoId) return;
    void mutate(
      () => client.createCodeTrigger(repoId, condition, action),
      upsert,
    );
  };

  const setEnabled = (trigger: CodeTriggerSnapshot, enabled: boolean) => {
    if (!repoId) return;
    void mutate(
      () => client.setCodeTriggerEnabled(repoId, trigger.id, enabled),
      upsert,
    );
  };

  const changeAction = (
    trigger: CodeTriggerSnapshot,
    action: CodeTriggerAction,
  ) => {
    if (!repoId) return;
    void mutate(
      () => client.createCodeTrigger(repoId, trigger.condition, action),
      upsert,
    );
  };

  const remove = (trigger: CodeTriggerSnapshot) => {
    if (!repoId) return;
    void mutate(
      () => client.deleteCodeTrigger(repoId, trigger.id),
      () =>
        setTriggers((current) =>
          current.filter((item) => item.id !== trigger.id),
        ),
    );
  };

  return (
    <div className="rounded-lg border border-border-subtle bg-background p-4">
      <div className="mb-4 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="truncate text-xs font-medium text-muted-foreground">
            {repository.name_with_owner}
          </p>
        </div>
        {loading && <LoaderCircle className="size-4 animate-spin" />}
      </div>
      {error && (
        <div className="notice-surface notice-critical mb-4 flex items-center justify-between gap-3 rounded-md border px-3 py-2 text-xs">
          <span className="flex items-start gap-2">
            <CircleAlert className="mt-0.5 size-3.5 shrink-0" />
            {error}
          </span>
          <Button
            type="button"
            size="xs"
            variant="outline"
            disabled={loading}
            onClick={() => void load()}
          >
            Try again
          </Button>
        </div>
      )}
      {!loading && repoId && (
        <CodeTriggerRules
          triggers={triggers}
          target={target}
          busy={busy}
          onArm={arm}
          onSetEnabled={setEnabled}
          onChangeAction={changeAction}
          onDelete={remove}
        />
      )}
    </div>
  );
}
