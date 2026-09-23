import { Redirect, useRouter } from "expo-router";
import { Pressable, Text, View } from "react-native";
import { Screen, Body } from "../src/components/Screen";
import { landingRoute } from "../src/lib/sections";
import { useActiveConnection } from "../src/session/store";

export default function WelcomeScreen() {
  const router = useRouter();
  const connection = useActiveConnection();

  if (connection?.machine) {
    // Land on the hub so every surface is a push with a back affordance;
    // redirecting into an index surface leaves it as the root of the stack
    // with no way back out.
    return <Redirect href="/home" />;
  }
  if (connection) {
    return <Redirect href={landingRoute(connection)} />;
  }

  return (
    <Screen title="Tidebreak">
      <Body>
        This is a preview. Use it to watch sessions, answer approvals, and
        steer runs on a Model Gateway deployment or a Tidebreak machine you
        host yourself. A local-only desktop install has nothing this phone can
        reach.
      </Body>
      <Body>
        You need either a gateway URL you can sign in to, or a machine URL and
        token from a self-hosted Tidebreak. Pair once; the phone keeps the
        connection on this device.
      </Body>
      <Pressable
        className="rounded-lg bg-primary px-4 py-3"
        onPress={() => router.push("/pair")}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          Pair a gateway
        </Text>
      </Pressable>
      <Pressable
        className="rounded-lg border border-border bg-background px-4 py-3"
        onPress={() => router.push("/attach-machine")}
      >
        <Text className="text-center text-base font-medium text-foreground">
          Connect your own machine
        </Text>
      </Pressable>
      <View />
    </Screen>
  );
}
