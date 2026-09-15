import "../src/global.css";
import { Stack } from "expo-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StatusBar } from "expo-status-bar";
import { useEffect, useState } from "react";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { PushSync } from "../src/push/registration";
import { connections, hydrateConnections } from "../src/session/runtime";
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
    void hydrateConnections().then((snapshot) => {
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
        <PushSync />
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
          <Stack.Screen name="scan" options={{ title: "Scan to pair" }} />
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
          <Stack.Screen name="console" options={{ title: "Gateway" }} />
          <Stack.Screen name="sandboxes" options={{ title: "Sandboxes" }} />
          <Stack.Screen name="sandbox/[id]" options={{ title: "Sandbox" }} />
          <Stack.Screen name="activity" options={{ title: "Activity" }} />
          <Stack.Screen name="catalog" options={{ title: "Models & apps" }} />
          <Stack.Screen name="limits" options={{ title: "My limits" }} />
          <Stack.Screen
            name="subscriptions"
            options={{ title: "Subscriptions" }}
          />
          <Stack.Screen
            name="shared-apps/index"
            options={{ title: "Shared apps" }}
          />
          <Stack.Screen
            name="shared-apps/[id]"
            options={{ title: "Shared app" }}
          />
          <Stack.Screen
            name="conversation/[id]"
            options={{ title: "Conversation" }}
          />
          {/* Administration. Registered unconditionally — the routes exist for
              every session and `AdminGate` is what answers a member who
              reaches one, so navigation never has to be rebuilt when the
              gateway confirms the role. */}
          <Stack.Screen name="admin/usage" options={{ title: "Usage" }} />
          <Stack.Screen
            name="admin/models"
            options={{ title: "Models & providers" }}
          />
          <Stack.Screen name="admin/people" options={{ title: "People" }} />
          <Stack.Screen name="admin/teams" options={{ title: "Teams" }} />
          <Stack.Screen name="admin/limits" options={{ title: "Limits" }} />
          <Stack.Screen
            name="admin/guardrails"
            options={{ title: "Guardrails" }}
          />
          <Stack.Screen name="admin/audit" options={{ title: "Audit log" }} />
          <Stack.Screen
            name="admin/configuration"
            options={{ title: "Configuration" }}
          />
        </Stack>
      </QueryClientProvider>
    </GestureHandlerRootView>
  );
}
