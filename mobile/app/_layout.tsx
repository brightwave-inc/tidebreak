import "../src/global.css";
import { Stack } from "expo-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StatusBar } from "expo-status-bar";
import { useEffect, useState } from "react";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { connections } from "../src/session/runtime";
import { useConnectionStore } from "../src/session/store";

const queryClient = new QueryClient();

export default function RootLayout() {
  const setHydrated = useConnectionStore((state) => state.setHydrated);
  const apply = useConnectionStore((state) => state.apply);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    // Every change to the connection set lands here: pairing, switching,
    // signing out of one connection, and a gateway revoking one session's
    // refresh family.
    const stop = connections.onChange(apply);
    void connections.hydrate().then((snapshot) => {
      setHydrated(snapshot);
      setReady(true);
    });
    return stop;
  }, [setHydrated, apply]);

  if (!ready) {
    return null;
  }

  return (
    <GestureHandlerRootView style={{ flex: 1 }}>
      <QueryClientProvider client={queryClient}>
        <StatusBar style="auto" />
        <Stack
          screenOptions={{
            headerShadowVisible: false,
            headerStyle: { backgroundColor: "#f4f5f7" },
            contentStyle: { backgroundColor: "#f4f5f7" },
            // Chevron-only back button; the default inherits the previous
            // route's title and falls back to its file name ("index").
            headerBackButtonDisplayMode: "minimal",
          }}
        >
          <Stack.Screen name="index" options={{ headerShown: false, title: "Tidebreak" }} />
          <Stack.Screen name="pair" options={{ title: "Gateway" }} />
          <Stack.Screen name="attach" options={{ title: "Machine" }} />
          <Stack.Screen name="home" options={{ title: "Home" }} />
          <Stack.Screen
            name="workspace/[id]/start"
            options={{ title: "Start session" }}
          />
          <Stack.Screen name="chats" options={{ title: "Chats" }} />
          <Stack.Screen name="chat/[id]" options={{ title: "Chat" }} />
          <Stack.Screen name="delivery" options={{ title: "Delivery" }} />
          <Stack.Screen name="sessions" options={{ title: "Sessions" }} />
          <Stack.Screen name="session/[id]" options={{ title: "Session" }} />
          <Stack.Screen name="approvals" options={{ title: "Approvals" }} />
          <Stack.Screen name="settings" options={{ title: "Settings" }} />
          <Stack.Screen name="connections" options={{ title: "Connections" }} />
        </Stack>
      </QueryClientProvider>
    </GestureHandlerRootView>
  );
}
