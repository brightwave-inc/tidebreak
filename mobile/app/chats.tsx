import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useIsFocused, useRouter } from "expo-router";
import { useRef, useState } from "react";
import {
  Pressable,
  RefreshControl,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Button, LoadingState } from "../src/components/Controls";
import { Body, ErrorText } from "../src/components/Screen";
import {
  createMobileChat,
  listMobileChats,
  type MobileChat,
} from "../src/lib/chatApi";
import { useSessionStore } from "../src/session/store";
import { useMachineClient } from "../src/session/useMachineClient";

export default function ChatsScreen() {
  const router = useRouter();
  const isFocused = useIsFocused();
  const queryClient = useQueryClient();
  const machine = useSessionStore((state) => state.session?.machine);
  const client = useMachineClient();
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const creatingRef = useRef(false);
  const queryKey = ["mobile-chats", machine?.baseUrl] as const;
  const chatsQuery = useQuery({
    queryKey,
    enabled: !!client && isFocused,
    queryFn: () => listMobileChats(client!),
  });

  async function createChat() {
    if (!client || creatingRef.current) return;
    creatingRef.current = true;
    setCreating(true);
    setCreateError(null);
    try {
      const chat = await createMobileChat(client);
      queryClient.setQueryData<MobileChat[]>(queryKey, (current) => [
        chat,
        ...(current ?? []).filter((item) => item.id !== chat.id),
      ]);
      router.push({
        pathname: "/chat/[id]",
        params: { id: chat.id, title: chat.title || "New work" },
      });
    } catch (error) {
      setCreateError(
        error instanceof Error ? error.message : "The chat could not be created.",
      );
    } finally {
      creatingRef.current = false;
      setCreating(false);
    }
  }

  if (!machine || !client) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <Text className="text-2xl font-semibold text-foreground">Chats</Text>
        <View className="mt-3">
          <Body>Attach a Tidebreak machine to use chats.</Body>
        </View>
        <View className="mt-4">
          <Button label="Attach" onPress={() => router.replace("/attach")} />
        </View>
      </SafeAreaView>
    );
  }

  const chats = chatsQuery.data ?? [];

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView
        contentContainerClassName="gap-3 px-5 py-6"
        refreshControl={
          <RefreshControl
            refreshing={chatsQuery.isRefetching}
            onRefresh={() => void chatsQuery.refetch()}
          />
        }
      >
        <View className="flex-row items-center justify-between gap-3">
          <View className="flex-1">
            <Text className="text-2xl font-semibold text-foreground">Chats</Text>
            <Text className="mt-1 text-sm text-muted-foreground">
              Send a message without opening the desktop app.
            </Text>
          </View>
          <Button
            label="New chat"
            compact
            busy={creating}
            onPress={() => void createChat()}
          />
        </View>

        {createError ? <ErrorText>{createError}</ErrorText> : null}
        {chatsQuery.isLoading ? <LoadingState label="Loading chats…" /> : null}
        {chatsQuery.isError ? (
          <ErrorText>
            {chatsQuery.error instanceof Error
              ? chatsQuery.error.message
              : "The chat list could not be loaded."}
          </ErrorText>
        ) : null}
        {!chatsQuery.isLoading && !chatsQuery.isError && chats.length === 0 ? (
          <View className="rounded-xl border border-border bg-background p-4">
            <Text className="text-base font-medium text-foreground">
              No chats yet
            </Text>
            <Text className="mt-1 text-sm text-muted-foreground">
              Start a chat, then send its first message from your phone.
            </Text>
          </View>
        ) : null}

        {chats.map((chat) => (
          <Pressable
            key={chat.id}
            accessibilityRole="button"
            accessibilityLabel={`Open ${chat.title?.trim() || "new work"}`}
            className="gap-1 rounded-xl border border-border bg-background p-4"
            onPress={() =>
              router.push({
                pathname: "/chat/[id]",
                params: { id: chat.id, title: chat.title || "New work" },
              })
            }
          >
            <Text className="text-base font-medium text-foreground">
              {chat.title?.trim() || "New work"}
            </Text>
            <Text
              className="font-mono text-xs text-muted-foreground"
              numberOfLines={1}
            >
              {chat.model || "Default model"}
            </Text>
            <Text className="text-xs text-muted-foreground">
              {formatChatDate(chat.created_at)}
            </Text>
          </Pressable>
        ))}
      </ScrollView>
    </SafeAreaView>
  );
}

function formatChatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}
