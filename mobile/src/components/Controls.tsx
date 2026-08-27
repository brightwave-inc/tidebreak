import type { ReactNode } from "react";
import {
  ActivityIndicator,
  Pressable,
  Text,
  TextInput,
  type TextInputProps,
  View,
} from "react-native";

type ButtonVariant = "primary" | "secondary" | "destructive";

const BUTTON_CLASSES: Record<ButtonVariant, string> = {
  primary: "border-primary bg-primary",
  secondary: "border-border bg-background",
  destructive: "border-critical bg-critical",
};

const LABEL_CLASSES: Record<ButtonVariant, string> = {
  primary: "text-primary-foreground",
  secondary: "text-foreground",
  destructive: "text-primary-foreground",
};

export function Button({
  label,
  onPress,
  disabled = false,
  busy = false,
  variant = "primary",
  compact = false,
  accessibilityLabel,
}: {
  label: string;
  onPress: () => void;
  disabled?: boolean;
  busy?: boolean;
  variant?: ButtonVariant;
  compact?: boolean;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel ?? label}
      accessibilityState={{ disabled: disabled || busy, busy }}
      disabled={disabled || busy}
      className={`min-h-11 flex-row items-center justify-center gap-2 rounded-lg border px-4 ${
        compact ? "py-2" : "py-3"
      } ${BUTTON_CLASSES[variant]} disabled:opacity-50`}
      onPress={onPress}
    >
      {busy ? <ActivityIndicator size="small" /> : null}
      <Text className={`text-sm font-medium ${LABEL_CLASSES[variant]}`}>
        {label}
      </Text>
    </Pressable>
  );
}

export function Field({
  label,
  hint,
  multiline = false,
  ...props
}: TextInputProps & { label: string; hint?: string }) {
  return (
    <View className="gap-1.5">
      <Text className="text-sm font-medium text-foreground">{label}</Text>
      {hint ? <Text className="text-xs text-muted-foreground">{hint}</Text> : null}
      <TextInput
        {...props}
        multiline={multiline}
        placeholderTextColor="#697386"
        textAlignVertical={multiline ? "top" : "center"}
        className={`rounded-lg border border-border bg-background px-3 text-base text-foreground ${
          multiline ? "min-h-24 py-3" : "min-h-12 py-2.5"
        }`}
      />
    </View>
  );
}

export function StatusPill({
  children,
  tone = "neutral",
}: {
  children: ReactNode;
  tone?: "neutral" | "live" | "warning" | "info" | "success" | "critical";
}) {
  const style = {
    neutral: {
      container: "border-border bg-muted",
      text: "text-muted-foreground",
    },
    live: {
      container: "border-live-border bg-live-background",
      text: "text-live-foreground",
    },
    warning: {
      container: "border-warning-border bg-warning-background",
      text: "text-warning-foreground",
    },
    info: {
      container: "border-info-border bg-info-background",
      text: "text-info-foreground",
    },
    success: {
      container: "border-success-border bg-success-background",
      text: "text-success-foreground",
    },
    critical: {
      container: "border-critical-border bg-critical-background",
      text: "text-critical-foreground",
    },
  }[tone];
  return (
    <View
      className={`self-start rounded-full border px-2.5 py-1 ${style.container}`}
    >
      <Text className={`text-xs font-medium ${style.text}`}>{children}</Text>
    </View>
  );
}

export function LoadingState({ label }: { label: string }) {
  return (
    <View className="items-center gap-2 py-12">
      <ActivityIndicator />
      <Text className="text-sm text-muted-foreground">{label}</Text>
    </View>
  );
}

export function SectionLabel({ children }: { children: ReactNode }) {
  return (
    <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
      {children}
    </Text>
  );
}
