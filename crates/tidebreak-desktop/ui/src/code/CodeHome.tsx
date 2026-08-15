import { useEffect, useState, type FormEvent } from "react";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";

import { useApp } from "@/AppContext";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { RouteFrame } from "@/RouteFrame";
import { friendlyErrorMessage } from "@/lib/utils";
import { useCodeCatalogStore } from "./CodeCatalogStore";
import { CodeSidebar } from "./CodeSidebar";
import { DoctorList } from "./DoctorList";
import { isHarnessReady } from "./labels";

/**
 * `/code` home: the doctor when no engine is usable, otherwise repo
 * registration and the registered list.
 *
 * A reader who has not installed or signed in to a harness cannot start a
 * workspace, so the empty state is the remediation the doctor already wrote
 * rather than a blank register form.
 */

export function CodeHome() {
  return (
    <RouteFrame sidebar={<CodeSidebar />}>
      <div className="content-container min-h-0 w-full min-w-0 flex-1 overflow-auto">
        <CodeHomeBody />
      </div>
    </RouteFrame>
  );
}

function CodeHomeBody() {
  const navigate = useNavigate();
  const { client } = useApp();
  const doctor = useCodeCatalogStore((state) => state.doctor);
  const repos = useCodeCatalogStore((state) => state.repos);
  const loaded = useCodeCatalogStore((state) => state.loaded);
  const error = useCodeCatalogStore((state) => state.error);
  const refresh = useCodeCatalogStore((state) => state.refresh);
  const upsertRepo = useCodeCatalogStore((state) => state.upsertRepo);
  const [path, setPath] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [registering, setRegistering] = useState(false);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    void refresh(client);
  }, [client, refresh]);

  const ready = doctor?.harnesses.some(isHarnessReady) ?? false;

  async function onRefresh() {
    setRefreshing(true);
    try {
      await refresh(client);
    } finally {
      setRefreshing(false);
    }
  }

  async function register(event: FormEvent) {
    event.preventDefault();
    if (!path.trim()) return;
    setRegistering(true);
    try {
      const repo = await client.createCodeRepo({
        path: path.trim(),
        display_name: displayName.trim() || undefined,
      });
      upsertRepo(repo);
      setPath("");
      setDisplayName("");
      await navigate({ to: "/code/r/$repoId", params: { repoId: repo.id } });
    } catch (err) {
      toast.error(friendlyErrorMessage(err, "Could not register that repo"));
    } finally {
      setRegistering(false);
    }
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-col gap-8 px-6 py-8">
      <div>
        <h1 className="text-2xl font-medium tracking-tight">Code</h1>
        <p className="text-muted-foreground text-sm">
          Register a local git repository, then open isolated workspaces on it.
        </p>
      </div>
      {error && <p className="text-sm text-critical">{error}</p>}
      {!loaded && <p className="text-muted-foreground text-sm">Loading…</p>}
      {doctor && !ready && (
        <section className="flex flex-col gap-3">
          <h2 className="text-lg font-semibold">Install a coding harness</h2>
          <p className="text-muted-foreground text-sm">
            No harness is installed and signed in. Code mode drives an engine
            already on this machine — install one, sign in from your own
            terminal, then refresh.
          </p>
          <DoctorList
            report={doctor}
            onRefresh={() => void onRefresh()}
            refreshing={refreshing}
          />
        </section>
      )}
      {ready && (
        <>
          <section className="flex flex-col gap-3">
            <h2 className="text-lg font-semibold">Register a repo</h2>
            <form className="flex flex-col gap-3" onSubmit={register}>
              <label className="flex flex-col gap-1 text-sm">
                <span className="font-medium">Path</span>
                <Input
                  value={path}
                  onChange={(event) => setPath(event.target.value)}
                  placeholder="/Users/you/src/app"
                  disabled={registering}
                />
              </label>
              <label className="flex flex-col gap-1 text-sm">
                <span className="font-medium">Display name</span>
                <Input
                  value={displayName}
                  onChange={(event) => setDisplayName(event.target.value)}
                  disabled={registering}
                />
              </label>
              <Button type="submit" disabled={registering || !path.trim()} className="self-start">
                {registering ? "Registering…" : "Register"}
              </Button>
            </form>
          </section>
          <section className="flex flex-col gap-3">
            <h2 className="text-lg font-semibold">Repos</h2>
            {repos.length === 0 ? (
              <p className="text-muted-foreground text-sm">None registered yet.</p>
            ) : (
              <ul className="flex flex-col gap-1">
                {repos.map((repo) => (
                  <li key={repo.id}>
                    <button
                      type="button"
                      className="hover:bg-muted w-full rounded-md px-3 py-2 text-left text-sm"
                      onClick={() =>
                        void navigate({
                          to: "/code/r/$repoId",
                          params: { repoId: repo.id },
                        })
                      }
                    >
                      <span className="font-medium">{repo.display_name}</span>
                      <span className="text-muted-foreground ml-2 font-mono text-xs">
                        {repo.root_path}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </>
      )}
    </div>
  );
}
