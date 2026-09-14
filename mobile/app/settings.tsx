import * as Application from "expo-application";
import { useRouter } from "expo-router";
import * as Updates from "expo-updates";
import { useCallback, useEffect, useState } from "react";
import { Pressable, Text, View } from "react-native";
import { Button } from "../src/components/Controls";
import { Screen, Body } from "../src/components/Screen";
import { connections } from "../src/session/runtime";
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
    <View className="rounded-xl border border-border bg-background p-4 gap-2">
      <Text className="text-xs uppercase tracking-wide text-muted-foreground">
        About this app
      </Text>
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
    </View>
  );
}

export default function SettingsScreen() {
  const router = useRouter();
  const connection = useActiveConnection();
  const count = useConnectionStore((state) => state.connections.length);

  /**
   * Sign-out is per connection: this one's credential is deleted and its
   * record forgotten, every other connection keeps its session, and the app
   * lands wherever the survivors put it.
   */
  async function signOut() {
    await connections.removeActive();
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
      <View className="rounded-xl border border-border bg-background p-4 gap-2">
        <Text className="text-xs uppercase tracking-wide text-muted-foreground">
          Machine
        </Text>
        <Text className="text-base text-foreground">
          {connection?.machine?.baseUrl ?? "Not attached"}
        </Text>
        {connection?.machine?.resource ? (
          <Text className="text-xs text-muted-foreground">
            {connection.machine.resource}
          </Text>
        ) : null}
      </View>
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
