import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Crypto from "expo-crypto";
import {
  useIsFocused,
  useLocalSearchParams,
  useRouter,
} from "expo-router";
import { useEffect, useRef, useState } from "react";
import {
  KeyboardAvoidingView,
  Platform,
  RefreshControl,
  ScrollView,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { Button, LoadingState } from "../../src/components/Controls";
import { ErrorText } from "../../src/components/Screen";
import {
  addOptimisticMobileChatQueuedTurn,
  chatTurnIdentityForDraft,
  getMobileChat,
  getMobileChatTranscript,
  listMobileChatQueuedTurns,
  postMobileChatMessage,
  type MobileChatMessage,
  type MobileChatQueue,
  type MobileChatQueuedTurn,
  type MobileChatTurnIdentity,
} from "../../src/lib/chatApi";
import { useSessionStore } from "../../src/session/store";
import { useMachineClient } from "../../src/session/useMachineClient";

export default function ChatDetailScreen() {
  const router = useRouter();
  const isFocused = useIsFocused();
  const params = useLocalSearchParams<{ id?: string; title?: string }>();
  const machine = useSessionStore((state) => state.session?.machine);
  const client = useMachineClient();
  const queryClient = useQueryClient();
  const scrollRef = useRef<ScrollView>(null);
  const sendingRef = useRef(false);
  const [message, setMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [receipt, setReceipt] = useState<string | null>(null);
  const [pendingIdentity, setPendingIdentity] =
    useState<MobileChatTurnIdentity | null>(null);
  const chatId = params.id;
  const machineKey = machine?.baseUrl;
  const chatQuery = useQuery({
    queryKey: ["mobile-chat", machineKey, chatId],
    enabled: !!client && !!chatId && isFocused,
    queryFn: () => getMobileChat(client!, chatId!),
  });
  const transcriptQuery = useQuery({
    queryKey: ["mobile-chat-transcript", machineKey, chatId],
    enabled: !!client && !!chatId && isFocused,
    queryFn: () => getMobileChatTranscript(client!, chatId!),
    refetchInterval: isFocused ? 3_000 : false,
  });
  const queueKey = ["mobile-chat-queue", machineKey, chatId] as const;
  const queueQuery = useQuery({
    queryKey: queueKey,
    enabled: !!client && !!chatId && isFocused,
    queryFn: () => listMobileChatQueuedTurns(client!, chatId!),
    refetchInterval: isFocused ? 3_000 : false,
  });
  const messages = transcriptQuery.data?.messages ?? [];
  const queued = queueQuery.data?.queued ?? [];

  useEffect(() => {
    scrollRef.current?.scrollToEnd({ animated: true });
  }, [messages.length, queued.length]);

  async function refreshMessages() {
    await Promise.all([transcriptQuery.refetch(), queueQuery.refetch()]);
  }

  async function sendMessage() {
    const content = message.trim();
    if (!client || !chatId || !content || sendingRef.current) return;
    const identity = chatTurnIdentityForDraft(
      pendingIdentity,
      content,
      Crypto.randomUUID,
    );
    setPendingIdentity(identity);
    sendingRef.current = true;
    setSending(true);
    setSendError(null);
    setReceipt(null);
    try {
      await postMobileChatMessage(
        client,
        chatId,
        identity.turnId,
        identity.content,
      );
      await queryClient.cancelQueries({ queryKey: queueKey });
      queryClient.setQueryData<MobileChatQueue>(queueKey, (current) =>
        addOptimisticMobileChatQueuedTurn(
          current,
          chatId,
          identity,
          new Date().toISOString(),
        ),
      );
      setMessage("");
      setPendingIdentity(null);
      setReceipt("Message accepted. Responses refresh automatically.");
      await queryClient.invalidateQueries({
        queryKey: ["mobile-chat-transcript", machineKey, chatId],
      });
      await queryClient.invalidateQueries({
        queryKey: ["mobile-chats", machineKey],
      });
    } catch (error) {
      setSendError(
        error instanceof Error ? error.message : "The message could not be sent.",
      );
    } finally {
      sendingRef.current = false;
      setSending(false);
    }
  }

  if (!machine || !client || !chatId) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <ErrorText>Attach a machine before you open this chat.</ErrorText>
        <View className="mt-4">
          <Button label="Go back" onPress={() => router.replace("/chats")} />
        </View>
      </SafeAreaView>
    );
  }

  const title = chatQuery.data?.title?.trim() || params.title || "New work";

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <KeyboardAvoidingView
        className="flex-1"
        behavior={Platform.OS === "ios" ? "padding" : undefined}
        keyboardVerticalOffset={Platform.OS === "ios" ? 74 : 0}
      >
        <View className="border-b border-border px-5 py-3">
          <Text
            className="text-lg font-semibold text-foreground"
            numberOfLines={2}
          >
            {title}
          </Text>
          <Text
            className="mt-0.5 font-mono text-xs text-muted-foreground"
            numberOfLines={1}
          >
            {chatQuery.data?.model || "Default model"}
          </Text>
        </View>

        <ScrollView
          ref={scrollRef}
          className="flex-1"
          contentContainerClassName="gap-3 px-5 py-4"
          keyboardShouldPersistTaps="handled"
          refreshControl={
            <RefreshControl
              refreshing={
                transcriptQuery.isRefetching || queueQuery.isRefetching
              }
              onRefresh={() => void refreshMessages()}
            />
          }
        >
          {chatQuery.isError ? (
            <ErrorText>
              {chatQuery.error instanceof Error
                ? chatQuery.error.message
                : "The chat could not be loaded."}
            </ErrorText>
          ) : null}
          {transcriptQuery.isLoading ? (
            <LoadingState label="Loading messages…" />
          ) : null}
          {transcriptQuery.isError ? (
            <ErrorText>
              {transcriptQuery.error instanceof Error
                ? transcriptQuery.error.message
                : "The messages could not be loaded."}
            </ErrorText>
          ) : null}
          {queueQuery.isError ? (
            <ErrorText>
              {queueQuery.error instanceof Error
                ? queueQuery.error.message
                : "The queued messages could not be loaded."}
            </ErrorText>
          ) : null}
          {!transcriptQuery.isLoading &&
          !transcriptQuery.isError &&
          !queueQuery.isLoading &&
          !queueQuery.isError &&
          messages.length === 0 &&
          queued.length === 0 ? (
            <View className="rounded-xl border border-border bg-background p-4">
              <Text className="text-base font-medium text-foreground">
                Start the conversation
              </Text>
              <Text className="mt-1 text-sm text-muted-foreground">
                Send the first message from the composer.
              </Text>
            </View>
          ) : null}
          {messages.map((item) => (
            <ChatMessage key={item.id} message={item} />
          ))}
          {queued.map((turn) => (
            <QueuedChatMessage
              key={turn.id}
              turn={turn}
              paused={queueQuery.data?.paused ?? false}
            />
          ))}
        </ScrollView>

        <View className="gap-2 border-t border-border bg-background px-4 py-3">
          {receipt ? (
            <Text className="text-xs text-success-foreground">{receipt}</Text>
          ) : null}
          {sendError ? <ErrorText>{sendError}</ErrorText> : null}
          <TextInput
            multiline
            value={message}
            editable={!sending}
            accessibilityLabel="Message"
            placeholder="Message this chat"
            placeholderTextColor="#697386"
            textAlignVertical="top"
            className="max-h-40 min-h-20 rounded-xl border border-border bg-page-background px-3 py-3 text-base text-foreground"
            onChangeText={(next) => {
              setMessage(next);
              setReceipt(null);
              setSendError(null);
            }}
          />
          <Button
            label="Send"
            busy={sending}
            disabled={!message.trim() || chatQuery.isError}
            onPress={() => void sendMessage()}
          />
        </View>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

function QueuedChatMessage({
  turn,
  paused,
}: {
  turn: MobileChatQueuedTurn;
  paused: boolean;
}) {
  return (
    <View
      accessible
      accessibilityLabel={paused ? "Queued message, paused" : "Queued message"}
      className="ml-8 rounded-xl border border-dashed border-info-border bg-info-background p-3"
    >
      <Text className="mb-1 text-xs font-medium uppercase tracking-wide text-info-foreground">
        {paused ? "Queued · Paused" : "Queued"}
      </Text>
      <Text className="text-base text-foreground" selectable>
        {turn.content}
      </Text>
    </View>
  );
}

function ChatMessage({ message }: { message: MobileChatMessage }) {
  if (message.role === "compaction") {
    return (
      <Text className="py-2 text-center text-xs text-muted-foreground">
        Earlier messages were summarized.
      </Text>
    );
  }
  const user = message.role === "user";
  const system = message.role === "system";
  return (
    <View
      className={`rounded-xl border p-3 ${
        user
          ? "ml-8 border-primary bg-primary"
          : system
            ? "border-info-border bg-info-background"
            : "mr-8 border-border bg-background"
      }`}
    >
      {system ? (
        <Text className="mb-1 text-xs font-medium uppercase tracking-wide text-info-foreground">
          System
        </Text>
      ) : null}
      <Text
        className={`text-base ${
          user ? "text-primary-foreground" : "text-foreground"
        }`}
        selectable
      >
        {message.content}
      </Text>
    </View>
  );
}
