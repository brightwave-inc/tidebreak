import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useRef, useState } from "react";
import {
  NativeScrollEvent,
  NativeSyntheticEvent,
  Pressable,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import { useMachineClient } from "../../src/session/useMachineClient";
import { useSessionEvents } from "../../src/session/useSessionEvents";
import { useSessionStore } from "../../src/session/store";
import type { TimelineItem } from "../../src/lib/transcript";

const PIN_THRESHOLD = 80;

export default function SessionDetailScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{
    id?: string;
    title?: string;
    workspace?: string;
  }>();
  const session = useSessionStore((state) => state.session);
  const client = useMachineClient();
  const transcript = useSessionEvents(client, params.id);
  const scrollRef = useRef<ScrollView>(null);
  const [pinned, setPinned] = useState(true);
  const [showJump, setShowJump] = useState(false);

  useEffect(() => {
    if (pinned) {
      scrollRef.current?.scrollToEnd({ animated: true });
    } else {
      setShowJump(true);
    }
  }, [transcript.items, pinned]);

  function onScroll(event: NativeSyntheticEvent<NativeScrollEvent>) {
    const { contentOffset, contentSize, layoutMeasurement } = event.nativeEvent;
    const fromBottom =
      contentSize.height - layoutMeasurement.height - contentOffset.y;
    const atBottom = fromBottom < PIN_THRESHOLD;
    setPinned(atBottom);
    if (atBottom) setShowJump(false);
  }

  if (!session?.machine) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <Text className="text-base text-muted-foreground">
          Attach a machine to read this session.
        </Text>
        <Pressable className="mt-4" onPress={() => router.replace("/")}>
          <Text className="text-base text-foreground">Go back</Text>
        </Pressable>
      </SafeAreaView>
    );
  }

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <View className="px-5 py-3 border-b border-border gap-1">
        <Text className="text-lg font-semibold text-foreground">
          {params.title || "Session"}
        </Text>
        <Text className="text-xs text-muted-foreground">
          {params.workspace ?? ""} · {transcript.live ? "Live" : "Reconnecting…"}
        </Text>
      </View>
      <View className="flex-1">
        <ScrollView
          ref={scrollRef}
          className="flex-1"
          contentContainerClassName="px-5 py-4 gap-3"
          onScroll={onScroll}
          scrollEventThrottle={16}
        >
          {transcript.items.length === 0 ? (
            <Text className="text-sm text-muted-foreground">
              Waiting for events…
            </Text>
          ) : (
            transcript.items.map((item) => <TimelineRow key={item.id} item={item} />)
          )}
        </ScrollView>
        {showJump && !pinned ? (
          <Pressable
            className="absolute bottom-4 self-center rounded-full bg-primary px-4 py-2"
            style={{ alignSelf: "center", left: "50%", marginLeft: -70 }}
            onPress={() => {
              setPinned(true);
              setShowJump(false);
              scrollRef.current?.scrollToEnd({ animated: true });
            }}
          >
            <Text className="text-sm text-primary-foreground">Jump to latest</Text>
          </Pressable>
        ) : null}
      </View>
    </SafeAreaView>
  );
}

function TimelineRow({ item }: { item: TimelineItem }) {
  if (item.kind === "user") {
    return (
      <View className="self-end max-w-[90%] rounded-xl bg-primary px-3 py-2">
        <Text className="text-sm text-primary-foreground">{item.text}</Text>
      </View>
    );
  }
  if (item.kind === "assistant") {
    return (
      <View className="self-start max-w-[90%] rounded-xl border border-border bg-background px-3 py-2">
        <Text className="text-sm text-foreground">{item.text}</Text>
      </View>
    );
  }
  if (item.kind === "tool") {
    return (
      <View className="rounded-lg border border-border bg-muted px-3 py-2">
        <Text className="text-sm text-foreground" numberOfLines={1}>
          {item.name}
          {item.summary ? ` · ${item.summary}` : ""}
        </Text>
      </View>
    );
  }
  return (
    <Text className="text-xs text-muted-foreground text-center">{item.text}</Text>
  );
}
