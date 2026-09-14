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
        Pair this phone once with a Model Gateway deployment. The app then
        attaches to the hosted Tidebreak machine that deployment advertises,
        using the same HTTP wire as desktop.
      </Body>
      <Body>
        Tokens stay in the device secure store, one credential per connection.
        Refresh tokens rotate, and this client only asks for the resources the
        gateway says it may hold.
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
