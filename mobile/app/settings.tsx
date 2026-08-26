import { useRouter } from "expo-router";
import { Pressable, Text, View } from "react-native";
import { Screen, Body } from "../src/components/Screen";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

export default function SettingsScreen() {
  const router = useRouter();
  const session = useSessionStore((state) => state.session);
  const signOutLocal = useSessionStore((state) => state.signOutLocal);

  async function signOut() {
    await tokenStore.clear();
    signOutLocal();
    router.replace("/");
  }

  return (
    <Screen title="Settings">
      <View className="rounded-xl border border-border bg-background p-4 gap-2">
        <Text className="text-xs uppercase tracking-wide text-muted-foreground">
          Gateway
        </Text>
        <Text className="text-base text-foreground">
          {session?.gatewayUrl ?? "Not paired"}
        </Text>
        {session?.installationId ? (
          <Text className="text-sm text-muted-foreground">
            Installation {session.installationId}
          </Text>
        ) : null}
      </View>
      <View className="rounded-xl border border-border bg-background p-4 gap-2">
        <Text className="text-xs uppercase tracking-wide text-muted-foreground">
          Machine
        </Text>
        <Text className="text-base text-foreground">
          {session?.machine?.baseUrl ?? "Not attached"}
        </Text>
        {session?.machine?.resource ? (
          <Text className="text-xs text-muted-foreground">
            {session.machine.resource}
          </Text>
        ) : null}
      </View>
      <Body>
        Sign out clears the rotating refresh token and every cached access
        token from the secure store.
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
