import "../src/global.css";
import { Stack } from "expo-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StatusBar } from "expo-status-bar";
import { useEffect, useState } from "react";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

const queryClient = new QueryClient();

export default function RootLayout() {
  const setHydrated = useSessionStore((state) => state.setHydrated);
  const signOutLocal = useSessionStore((state) => state.signOutLocal);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    const stop = tokenStore.onSignedOut(signOutLocal);
    void tokenStore.hydrate().then((session) => {
      setHydrated(session);
      setReady(true);
    });
    return stop;
  }, [setHydrated, signOutLocal]);

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
          }}
        >
          <Stack.Screen name="index" options={{ headerShown: false }} />
          <Stack.Screen name="pair" options={{ title: "Gateway" }} />
          <Stack.Screen name="attach" options={{ title: "Machine" }} />
          <Stack.Screen name="home" options={{ title: "Attached" }} />
          <Stack.Screen
            name="workspace/[id]/start"
            options={{ title: "Start session" }}
          />
          <Stack.Screen name="sessions" options={{ title: "Sessions" }} />
          <Stack.Screen name="session/[id]" options={{ title: "Session" }} />
          <Stack.Screen name="approvals" options={{ title: "Approvals" }} />
          <Stack.Screen name="settings" options={{ title: "Settings" }} />
        </Stack>
      </QueryClientProvider>
    </GestureHandlerRootView>
  );
}
