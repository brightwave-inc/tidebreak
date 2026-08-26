import { Redirect, useRouter } from "expo-router";
import { Pressable, Text, View } from "react-native";
import { Screen, Body } from "../src/components/Screen";
import { useSessionStore } from "../src/session/store";

export default function WelcomeScreen() {
  const router = useRouter();
  const session = useSessionStore((state) => state.session);

  if (session?.machine) {
    return <Redirect href="/home" />;
  }
  if (session) {
    return <Redirect href="/attach" />;
  }

  return (
    <Screen title="Tidebreak">
      <Body>
        Pair this phone once with a Model Gateway deployment. The app then
        attaches to the hosted Tidebreak machine that deployment advertises,
        using the same HTTP wire as desktop.
      </Body>
      <Body>
        Tokens stay in the device secure store. Refresh tokens rotate; this
        client never requests resources other than control and the machine
        itself.
      </Body>
      <Pressable
        className="rounded-lg bg-primary px-4 py-3"
        onPress={() => router.push("/pair")}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Pair a gateway
        </Text>
      </Pressable>
      <View />
    </Screen>
  );
}
