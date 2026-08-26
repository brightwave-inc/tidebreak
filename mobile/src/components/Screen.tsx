import { ReactNode } from "react";
import { ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

export function Screen({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <ScrollView contentContainerClassName="px-5 py-6 gap-4">
        <Text className="text-2xl font-semibold text-foreground">{title}</Text>
        <View className="gap-4">{children}</View>
      </ScrollView>
    </SafeAreaView>
  );
}

export function Body({ children }: { children: ReactNode }) {
  return <Text className="text-base leading-6 text-muted-foreground">{children}</Text>;
}

export function ErrorText({ children }: { children: ReactNode }) {
  return (
    <View className="rounded-lg border border-critical-border bg-critical-background p-3">
      <Text className="text-sm text-critical-foreground">{children}</Text>
    </View>
  );
}
