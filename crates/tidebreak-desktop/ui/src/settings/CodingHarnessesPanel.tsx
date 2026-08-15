import { useEffect, useState } from "react";

import type { ApiClient } from "../api/client";
import type { HarnessDoctorReport } from "../api/types";
import { DoctorList } from "@/code/DoctorList";
import { SettingsError, SettingsPanel } from "./primitives";

/**
 * Settings: the coding-harness doctor.
 *
 * Found, path, version, tier, capabilities, auth, remediation, and
 * unrecognized-event counts live here so a reader can repair an engine
 * without opening a workspace.
 */

export function CodingHarnessesPanel({ client }: { client: ApiClient }) {
  const [report, setReport] = useState<HarnessDoctorReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  async function load(refresh: boolean) {
    if (refresh) setRefreshing(true);
    else setLoading(true);
    setError(null);
    try {
      const next = refresh
        ? await client.refreshHarnessDoctor()
        : await client.getHarnessDoctor();
      setReport(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }

  useEffect(() => {
    void load(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client]);

  return (
    <SettingsPanel
      title="Coding harnesses"
      description="Engines installed on this machine, their versions, and what they can do."
      busy={loading || refreshing}
    >
      {error && <SettingsError>{error}</SettingsError>}
      {report && (
        <DoctorList
          report={report}
          onRefresh={() => void load(true)}
          refreshing={refreshing}
        />
      )}
    </SettingsPanel>
  );
}
