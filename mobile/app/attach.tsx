import { useRouter } from "expo-router";
import { useMemo, useState } from "react";
import { Pressable, Text, TextInput } from "react-native";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import { AttachError, discoverMachine, probePolicy } from "../src/lib/attach";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

type Stage = "idle" | "discover" | "verify" | "probe";

export default function AttachScreen() {
  const router = useRouter();
  const session = useSessionStore((state) => state.session);
  const setSession = useSessionStore((state) => state.setSession);
  const [url, setUrl] = useState(session?.machinePrefillUrl ?? "");
  const [stage, setStage] = useState<Stage>("idle");
  const [error, setError] = useState<string | null>(null);

  const hint = useMemo(() => {
    switch (stage) {
      case "discover":
        return "Reading /auth/discovery…";
      case "verify":
        return "Checking resource and gateway…";
      case "probe":
        return "Probing /policy with a machine token…";
      default:
        return null;
    }
  }, [stage]);

  async function attach() {
    if (!session) {
      router.replace("/");
      return;
    }
    setError(null);
    setStage("discover");
    try {
      const discovered = await discoverMachine(url, session.gatewayUrl);
      setStage("verify");
      setStage("probe");
      const token = await tokenStore.getAccessToken(discovered.resource);
      await probePolicy(discovered.baseUrl, token);
      await tokenStore.update({
        machine: {
          baseUrl: discovered.baseUrl,
          resource: discovered.resource,
        },
      });
      setSession(tokenStore.snapshot());
      router.replace("/home");
    } catch (err) {
      if (err instanceof AttachError) {
        setError(`${err.stage}: ${err.message}`);
      } else {
        setError(err instanceof Error ? err.message : "Attach failed.");
      }
    } finally {
      setStage("idle");
    }
  }

  return (
    <Screen title="Attach machine">
      <Body>
        Prefill comes from the gateway’s tidebreak_machine_url when the
        deployment hosts a machine. Discovery is untrusted until the resource
        derived from this URL matches the echo and the gateway URL matches the
        paired deployment.
      </Body>
      <TextInput
        autoCapitalize="none"
        autoCorrect={false}
        keyboardType="url"
        placeholder="https://tidebreak.example"
        placeholderTextColor="#6b7280"
        value={url}
        onChangeText={setUrl}
        className="rounded-lg border border-border bg-background px-3 py-3 text-base text-foreground"
      />
      {hint ? <Text className="text-sm text-info-foreground">{hint}</Text> : null}
      {error ? <ErrorText>{error}</ErrorText> : null}
      <Pressable
        disabled={stage !== "idle" || url.trim().length === 0}
        className="rounded-lg bg-primary px-4 py-3"
        onPress={() => void attach()}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Attach
        </Text>
      </Pressable>
    </Screen>
  );
}
