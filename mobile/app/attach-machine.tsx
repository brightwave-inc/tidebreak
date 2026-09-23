import { useRouter } from "expo-router";
import { useMemo, useState } from "react";
import { Pressable, Text, TextInput, View } from "react-native";
import { Button } from "../src/components/Controls";
import { QrScanner } from "../src/components/QrScanner";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import { AttachError } from "../src/lib/attach";
import { parseMachineQr } from "../src/lib/machineToken";
import {
  attachStandaloneMachine,
  machineTokenMessage,
  type StandaloneAttachStage,
} from "../src/lib/standaloneAttach";
import { REASON_REQUIRES_TLS } from "../src/lib/url";
import { connections } from "../src/session/runtime";
import { useThemeColors } from "../src/useThemeColors";

type Stage = "idle" | StandaloneAttachStage;

/**
 * Attaching a Tidebreak machine that runs without a gateway (#3404).
 *
 * The alternative onboarding path, not a second product: it is reached from
 * one line at the foot of the pair screen, and beyond the wording here nothing
 * about the app changes. What a person supplies is a URL and a token from the
 * machine's own roster, by paste or by the code an operator can render.
 *
 * The scanner is hosted on this screen rather than reached through `/scan`,
 * because this payload carries the credential and routing it would put a token
 * in the router's history (decision 98). It stays in this component's state
 * until it reaches the secure store.
 */
export default function AttachMachineScreen() {
  const colors = useThemeColors();
  const router = useRouter();
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const [scanning, setScanning] = useState(false);
  const [stage, setStage] = useState<Stage>("idle");
  const [error, setError] = useState<string | null>(null);
  const [tlsRefusal, setTlsRefusal] = useState(false);
  const busy = stage !== "idle";

  const hint = useMemo(() => {
    switch (stage) {
      case "discover":
        return "Reading /auth/discovery…";
      case "verify":
        return "Checking how this machine signs people in…";
      case "probe":
        return "Checking the token with the machine…";
      default:
        return null;
    }
  }, [stage]);

  // Shown while typing so a short paste is caught before a network round trip;
  // suppressed while the field is empty, which is not yet a mistake.
  const tokenHint = token.trim().length > 0 ? machineTokenMessage(token) : null;

  function onScanned(data: string) {
    const payload = parseMachineQr(data);
    if (!payload) {
      setError("That code is not a Tidebreak machine code.");
      return;
    }
    setUrl(payload.baseUrl);
    setToken(payload.token);
    setScanning(false);
    setError(null);
    setTlsRefusal(false);
  }

  async function attach() {
    setError(null);
    setTlsRefusal(false);
    try {
      const machine = await attachStandaloneMachine(url, token, {
        onStage: setStage,
      });
      await connections.addMachine({ machine, staticToken: token.trim() });
      // The token has reached the secure store; drop the copy this screen held.
      setToken("");
      router.replace("/home");
    } catch (err) {
      if (err instanceof AttachError && err.reason === REASON_REQUIRES_TLS) {
        setTlsRefusal(true);
        return;
      }
      setError(err instanceof Error ? err.message : "Could not attach.");
    } finally {
      setStage("idle");
    }
  }

  if (scanning) {
    return (
      <Screen title="Scan machine code">
        <QrScanner
          permissionPrompt="Tidebreak needs the camera to read the code your machine's operator gave you."
          hint="This code contains the machine's address and your token. Treat it like a password — it signs this phone in on its own."
          onScanned={onScanned}
        />
        {error ? <ErrorText>{error}</ErrorText> : null}
        <Button
          label="Enter it by hand instead"
          variant="secondary"
          onPress={() => {
            setScanning(false);
            setError(null);
          }}
        />
      </Screen>
    );
  }

  return (
    <Screen title="Your own Tidebreak">
      <Body>
        Attach a Tidebreak machine that runs on its own, with no Model Gateway
        behind it. You need its address and a token from the machine&apos;s
        token file — its operator issues both.
      </Body>
      <TextInput
        accessibilityLabel="Machine address"
        autoCapitalize="none"
        autoCorrect={false}
        keyboardType="url"
        placeholder="https://tidebreak.example"
        placeholderTextColor={colors.mutedForeground}
        value={url}
        onChangeText={setUrl}
        className="rounded-lg border border-border bg-background px-3 py-3 text-base text-foreground"
      />
      <TextInput
        accessibilityLabel="Machine token"
        autoCapitalize="none"
        autoCorrect={false}
        // The token is the credential: masked on screen, and kept out of the
        // keyboard's learned-word store.
        secureTextEntry
        textContentType="password"
        placeholder="Token"
        placeholderTextColor={colors.mutedForeground}
        value={token}
        onChangeText={setToken}
        className="rounded-lg border border-border bg-background px-3 py-3 text-base text-foreground"
      />
      {tokenHint ? (
        <Text className="text-sm text-muted-foreground">{tokenHint}</Text>
      ) : null}
      <Button
        disabled={busy}
        label="Scan a machine code"
        variant="secondary"
        onPress={() => {
          setScanning(true);
          setError(null);
        }}
      />
      {hint ? <Text className="text-sm text-info-foreground">{hint}</Text> : null}
      {tlsRefusal ? (
        <ErrorText>
          This phone will only send a token over HTTPS, or to a machine running
          on the phone itself. A machine on your network needs a certificate
          your device already trusts — Tidebreak cannot make an exception for a
          self-signed one.
        </ErrorText>
      ) : null}
      {error ? <ErrorText>{error}</ErrorText> : null}
      <Pressable
        accessibilityRole="button"
        disabled={busy || url.trim().length === 0 || token.trim().length === 0}
        className="rounded-lg bg-primary px-4 py-3 disabled:opacity-50"
        onPress={() => void attach()}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          {busy ? "Attaching…" : "Attach"}
        </Text>
      </Pressable>
      <View />
    </Screen>
  );
}
