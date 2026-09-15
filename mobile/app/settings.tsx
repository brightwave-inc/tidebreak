import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Application from "expo-application";
import { useRouter } from "expo-router";
import * as Updates from "expo-updates";
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { Pressable, Switch, Text, View } from "react-native";
import { Button } from "../src/components/Controls";
import { ConsoleLink } from "../src/components/Admin";
import { Screen, Body } from "../src/components/Screen";
import { connectionLabel } from "../src/lib/connections";
import { fetchGatewayMeta } from "../src/lib/gateway";
import {
  applyPushPreference,
  fetchPushPreferences,
  gatewayDeliversPush,
  pushKindCopy,
  setPushPreference,
  type PushPreference,
} from "../src/lib/push";
import { RESOURCE_CONTROL } from "../src/lib/resource";
import { administers } from "../src/lib/sections";
import { deregisterConnection } from "../src/push/registration";
import { readPushToken } from "../src/push/tokenCache";
import { connections, secureStorage } from "../src/session/runtime";
import { signOutActiveConnection } from "../src/session/signOut";
import { useActiveConnection, useConnectionStore } from "../src/session/store";

/** One labelled fact about this installation. */
function Fact({ label, value }: { label: string; value: string }) {
  return (
    <View className="flex-row items-baseline justify-between gap-3">
      <Text className="text-sm text-muted-foreground">{label}</Text>
      <Text
        className="flex-1 text-right text-sm text-foreground"
        numberOfLines={1}
        ellipsizeMode="middle"
      >
        {value}
      </Text>
    </View>
  );
}

function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <View className="rounded-xl border border-border bg-background p-4 gap-2">
      <Text className="text-xs uppercase tracking-wide text-muted-foreground">
        {title}
      </Text>
      {children}
    </View>
  );
}

/** A connection's `control` bearer, for the CLI-surface push routes. */
function controlToken(connectionId: string): Promise<string> {
  const store = connections.tokensFor(connectionId);
  if (!store) {
    return Promise.reject(new Error("That connection is no longer signed in."));
  }
  return store.getAccessToken(RESOURCE_CONTROL);
}

/**
 * Per-kind notification toggles.
 *
 * Rendered only against a gateway that advertises push, because a dark
 * installation answers these routes with a refusal rather than an empty list.
 * Suppression happens at the gateway's enqueue site, so a flipped switch
 * affects every device this account is signed into — said plainly below the
 * list, since a per-device reading of these controls would be wrong.
 */
function NotificationPreferences({
  connectionId,
  gatewayUrl,
}: {
  connectionId: string;
  gatewayUrl: string;
}) {
  const queryClient = useQueryClient();
  const queryKey = ["push-preferences", connectionId] as const;
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);

  const preferences = useQuery({
    queryKey,
    queryFn: async () =>
      fetchPushPreferences(gatewayUrl, await controlToken(connectionId)),
  });

  async function toggle(kind: string, enabled: boolean) {
    const previous = queryClient.getQueryData<PushPreference[]>(queryKey);
    // Optimistic: a Switch that snaps back a second later reads as broken.
    queryClient.setQueryData<PushPreference[]>(queryKey, (data) =>
      data ? applyPushPreference(data, kind, enabled) : data,
    );
    setPending(kind);
    setError(null);
    try {
      await setPushPreference(
        gatewayUrl,
        await controlToken(connectionId),
        kind,
        enabled,
      );
      await queryClient.invalidateQueries({ queryKey });
    } catch {
      if (previous) {
        queryClient.setQueryData(queryKey, previous);
      }
      setError("The gateway did not accept that change. Try again.");
    } finally {
      setPending(null);
    }
  }

  return (
    <Card title="Notifications">
      {preferences.data ? (
        <View className="gap-3">
          {preferences.data.map((preference) => {
            const copy = pushKindCopy(preference.kind);
            return (
              <View
                key={preference.kind}
                className="flex-row items-center justify-between gap-3"
              >
                <View className="min-w-0 flex-1">
                  <Text className="text-base text-foreground">
                    {copy.title}
                  </Text>
                  <Text className="text-xs text-muted-foreground">
                    {copy.detail}
                  </Text>
                </View>
                <Switch
                  disabled={pending !== null}
                  value={preference.enabled}
                  onValueChange={(enabled) =>
                    void toggle(preference.kind, enabled)
                  }
                />
              </View>
            );
          })}
          <Text className="pt-1 text-xs text-muted-foreground">
            Applies to your account on this gateway, across every device you
            are signed into.
          </Text>
        </View>
      ) : (
        <Text className="text-sm text-muted-foreground">
          {preferences.isError
            ? "Could not load notification settings."
            : "Loading…"}
        </Text>
      )}
      {error ? (
        <Text className="text-sm text-critical-foreground">{error}</Text>
      ) : null}
    </Card>
  );
}

/**
 * What this phone is registered to receive, per connection.
 *
 * Registration is per connection rather than per device — each gateway holds
 * its own push address for this phone — so leaving one gateway's notifications
 * is a per-row action and does not touch the others. Turning a row off deletes
 * the address at the gateway; the next foreground re-registers it while
 * permission stands, which is deliberate: this row manages the *device*, and
 * the durable "stop sending me this" control is the per-kind toggle above.
 */
function DeviceRegistrations() {
  const list = useConnectionStore((state) => state.connections);
  const [registered, setRegistered] = useState<Record<string, boolean>>({});
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    const entries = await Promise.all(
      list.map(
        async (connection) =>
          [
            connection.id,
            (await readPushToken(secureStorage, connection.id)) !== null,
          ] as const,
      ),
    );
    setRegistered(Object.fromEntries(entries));
  }, [list]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function turnOff(id: string, gatewayUrl: string) {
    setBusy(id);
    try {
      await deregisterConnection(id, gatewayUrl);
      await refresh();
    } finally {
      setBusy(null);
    }
  }

  if (list.length === 0) {
    return null;
  }

  return (
    <Card title="This device">
      {list.map((connection) => (
        <View
          key={connection.id}
          className="flex-row items-center justify-between gap-3 py-1"
        >
          <View className="min-w-0 flex-1">
            <Text className="text-base text-foreground" numberOfLines={1}>
              {connectionLabel(connection)}
            </Text>
            <Text className="text-xs text-muted-foreground">
              {registered[connection.id]
                ? "Registered for push"
                : "Not registered"}
            </Text>
          </View>
          {registered[connection.id] ? (
            <Button
              busy={busy === connection.id}
              compact
              label="Turn off"
              variant="secondary"
              accessibilityLabel={`Turn off notifications from ${connectionLabel(connection)}`}
              onPress={() =>
                void turnOff(connection.id, connection.gatewayUrl)
              }
            />
          ) : null}
        </View>
      ))}
      <Text className="pt-1 text-xs text-muted-foreground">
        Removes this phone&apos;s push address from that gateway. Signing out
        removes it too.
      </Text>
    </Card>
  );
}

/**
 * Whether this build can take an over-the-air update at all. Dev builds ship
 * with expo-updates disabled, so without this the card would offer a check
 * that can never succeed and report every dev launch as an error.
 */
const OTA_SUPPORTED = Updates.isEnabled && !__DEV__;

type OtaState =
  | "unsupported"
  | "checking"
  | "downloading"
  | "current"
  | "ready"
  | "error";

const OTA_STATUS: Record<OtaState, string> = {
  unsupported: "Over-the-air updates are off in this build.",
  checking: "Checking for a newer bundle…",
  downloading: "Downloading a newer bundle…",
  current: "Running the latest bundle for this build.",
  ready: "A newer bundle is downloaded. Restart to run it.",
  error: "Could not reach the update server.",
};

/**
 * The app's own identity: the binary you installed and the JS bundle running
 * inside it.
 *
 * Both rows are needed to answer "am I on the latest?". OTAs are routed by
 * channel and a fingerprint runtime version that any native change
 * invalidates (app.config.ts), so a build can sit on the newest bundle its
 * fingerprint allows while a newer bundle exists for a newer binary — the
 * version alone cannot say that, and the bundle id alone cannot either. The
 * check runs on open so the answer is already on screen, and the button
 * applies whatever it fetched.
 */
function AboutThisApp() {
  const [ota, setOta] = useState<OtaState>(
    OTA_SUPPORTED ? "checking" : "unsupported",
  );

  const check = useCallback(async () => {
    if (!OTA_SUPPORTED) {
      return;
    }
    setOta("checking");
    try {
      const result = await Updates.checkForUpdateAsync();
      // A rollback directive is also something to apply, and the check
      // reports it as isAvailable: false — testing availability alone would
      // call a pending rollback "up to date".
      if (!result.isAvailable && !result.isRollBackToEmbedded) {
        setOta("current");
        return;
      }
      setOta("downloading");
      await Updates.fetchUpdateAsync();
      setOta("ready");
    } catch {
      setOta("error");
    }
  }, []);

  useEffect(() => {
    void check();
  }, [check]);

  const appVersion = Application.nativeApplicationVersion ?? "—";
  const buildNumber = Application.nativeBuildVersion;
  // The build number is the EAS remote counter, not a repo value, so it is
  // the half of the version that actually distinguishes two shipped binaries.
  const version = buildNumber ? `${appVersion} (${buildNumber})` : appVersion;

  // The trailing 8 characters, not the leading 8. EAS update ids are UUIDv7,
  // so the leading hex digits are the top bits of a millisecond clock: any
  // two updates you actually want to tell apart share a prefix that reads as
  // identical at a glance. The last 8 are pure random bits, which is what
  // makes them scannable.
  const bundle = !OTA_SUPPORTED
    ? "—"
    : Updates.isEmbeddedLaunch
      ? "Embedded"
      : (Updates.updateId?.slice(-8) ?? "—");

  const busy = ota === "checking" || ota === "downloading";

  return (
    <Card title="About this app">
      <Fact label="Version" value={version} />
      <Fact label="OTA bundle" value={bundle} />
      <Text className="pt-1 text-xs text-muted-foreground">
        {OTA_STATUS[ota]}
      </Text>
      {ota === "unsupported" ? null : ota === "ready" ? (
        <Button
          label="Restart to apply"
          onPress={() => void Updates.reloadAsync()}
        />
      ) : (
        <Button
          busy={busy}
          label={ota === "error" ? "Try again" : "Check for updates"}
          variant="secondary"
          onPress={() => void check()}
        />
      )}
    </Card>
  );
}

export default function SettingsScreen() {
  const router = useRouter();
  const connection = useActiveConnection();
  const count = useConnectionStore((state) => state.connections.length);

  // Re-read rather than trusted from the connection record: an operator can
  // enable push long after a pairing was made, and the stored snapshot would
  // describe the installation as it was.
  const meta = useQuery({
    queryKey: ["meta", connection?.gatewayUrl ?? ""],
    queryFn: () => fetchGatewayMeta(connection?.gatewayUrl ?? ""),
    enabled: Boolean(connection?.gatewayUrl),
    staleTime: 60_000,
  });

  /**
   * Sign-out is per connection: this one's push registration is dropped at the
   * gateway first — it needs a bearer this connection is about to stop being
   * able to mint — then its credential is deleted and its record forgotten.
   * Every other connection keeps its session, and the app lands wherever the
   * survivors put it.
   */
  async function signOut() {
    await signOutActiveConnection();
    const next = connections.active();
    router.replace(next?.machine ? "/home" : next ? "/attach" : "/");
  }

  return (
    <Screen title="Settings">
      <Pressable
        accessibilityRole="button"
        accessibilityLabel="Manage connections"
        className="rounded-xl border border-border bg-background p-4 gap-2"
        onPress={() => router.push("/connections")}
      >
        <View className="flex-row items-center justify-between">
          <Text className="text-xs uppercase tracking-wide text-muted-foreground">
            Gateway
          </Text>
          <Text className="text-sm text-muted-foreground">
            {count > 1 ? `${count} connections  ›` : "Manage  ›"}
          </Text>
        </View>
        <Text className="text-base text-foreground">
          {connection?.gatewayUrl ?? "Not paired"}
        </Text>
        {connection?.installationId ? (
          <Text className="text-sm text-muted-foreground">
            Installation {connection.installationId}
          </Text>
        ) : null}
      </Pressable>
      <Card title="Machine">
        <Text className="text-base text-foreground">
          {connection?.machine?.baseUrl ?? "Not attached"}
        </Text>
        {connection?.machine?.resource ? (
          <Text className="text-xs text-muted-foreground">
            {connection.machine.resource}
          </Text>
        ) : null}
      </Card>
      {connection && gatewayDeliversPush(meta.data) ? (
        <NotificationPreferences
          connectionId={connection.id}
          gatewayUrl={connection.gatewayUrl}
        />
      ) : null}
      <DeviceRegistrations />
      {/* The deployment page the administration screens summarise. Gated on
          the role because the console redirects everyone else away from it,
          and a row that only ever lands on `/account` is worse than none. */}
      {administers(connection) ? (
        <View className="gap-2">
          <Body>
            This account administers this gateway. The phone summarises it;
            the console is where it is configured.
          </Body>
          <ConsoleLink webPath="/system" />
        </View>
      ) : null}
      <AboutThisApp />
      <Body>
        Sign out clears this connection&apos;s rotating refresh token and every
        cached access token from the secure store. Other connections stay
        signed in.
      </Body>
      <Pressable
        className="rounded-lg bg-critical px-4 py-3"
        onPress={() => void signOut()}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Sign out
        </Text>
      </Pressable>
    </Screen>
  );
}
