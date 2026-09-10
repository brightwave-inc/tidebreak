import { useLocalSearchParams, useRouter } from "expo-router";
import { useMemo, useState } from "react";
import { Pressable, Text, TextInput } from "react-native";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import {
  attachFailureFromParams,
  attachMachine,
  describeAttachFailure,
  type AttachFailure,
  type AttachStage,
} from "../src/lib/autoAttach";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

type Stage = "idle" | AttachStage;

export default function AttachScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{
    failure?: string | string[];
    detail?: string | string[];
  }>();
  const session = useSessionStore((state) => state.session);
  const setSession = useSessionStore((state) => state.setSession);
  const [url, setUrl] = useState(session?.machinePrefillUrl ?? "");
  const [stage, setStage] = useState<Stage>("idle");
  // Auto-attach hands its failure over in route params. Read once: a retry
  // owns the error state from then on, so the params must not resurrect it.
  const [failure, setFailure] = useState<AttachFailure | null>(() =>
    attachFailureFromParams(params),
  );

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
    setFailure(null);
    try {
      const machine = await attachMachine(url, session.gatewayUrl, {
        getAccessToken: (resource) => tokenStore.getAccessToken(resource),
        onStage: setStage,
      });
      await tokenStore.update({ machine });
      setSession(tokenStore.snapshot());
      router.replace("/home");
    } catch (err) {
      setFailure(describeAttachFailure(err));
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
      {failure?.kind === "unreachable" ? (
        <>
          <ErrorText>
            Couldn’t reach the machine. Hosted machines often sit on a private
            network — check that this phone is connected to the VPN or network
            the machine requires, then retry. ({failure.detail})
          </ErrorText>
          <Pressable
            disabled={stage !== "idle"}
            className="rounded-lg border border-border px-4 py-3"
            onPress={() => void attach()}
          >
            <Text className="text-center text-base font-medium text-foreground">
              Retry
            </Text>
          </Pressable>
        </>
      ) : null}
      {failure?.kind === "error" ? <ErrorText>{failure.message}</ErrorText> : null}
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
