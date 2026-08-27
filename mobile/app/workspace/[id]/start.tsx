import { useQuery } from "@tanstack/react-query";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  KeyboardAvoidingView,
  Platform,
  Pressable,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Button,
  Field,
  LoadingState,
  SectionLabel,
} from "../../../src/components/Controls";
import { ErrorText } from "../../../src/components/Screen";
import type {
  HarnessKind,
  PermissionMode,
} from "../../../src/generated/wire";
import {
  getCodePermissionPolicy,
  launchCodeSession,
  listActiveCodeWorkspaces,
  listCodeHarnessModels,
  listCodeHarnesses,
  type CodeHarnessOption,
} from "../../../src/lib/api";
import {
  defaultCreatePermissionMode,
  HARNESS_LABELS,
  harnessCanStartNow,
  harnessUnavailableReason,
  PERMISSION_MODE_DESCRIPTIONS,
  PERMISSION_MODE_LABELS,
  permissionModeWarning,
  permittedPermissionModes,
} from "../../../src/lib/codeLaunch";
import { useSessionDraftRecoveryStore } from "../../../src/session/draftRecovery";
import { useSessionStore } from "../../../src/session/store";
import { useMachineClient } from "../../../src/session/useMachineClient";

const NO_HARNESSES: CodeHarnessOption[] = [];

export default function StartWorkspaceSessionScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{ id?: string }>();
  const machine = useSessionStore((state) => state.session?.machine);
  const client = useMachineClient();
  const offerDraft = useSessionDraftRecoveryStore((state) => state.offer);
  const [selectedHarnessKind, setSelectedHarnessKind] =
    useState<HarnessKind | null>(null);
  const [modelsByHarness, setModelsByHarness] = useState<
    Partial<Record<HarnessKind, string>>
  >({});
  const [modesByHarness, setModesByHarness] = useState<
    Partial<Record<HarnessKind, PermissionMode>>
  >({});
  const [message, setMessage] = useState("");
  const [launching, setLaunching] = useState(false);
  const [launchError, setLaunchError] = useState<string | null>(null);
  const launchRef = useRef(false);
  const workspaceId = params.id;
  const machineKey = machine?.baseUrl;

  const workspacesQuery = useQuery({
    queryKey: ["code-workspaces", machineKey],
    enabled: !!client,
    queryFn: () => listActiveCodeWorkspaces(client!),
  });
  const harnessesQuery = useQuery({
    queryKey: ["code-harnesses", machineKey],
    enabled: !!client,
    queryFn: () => listCodeHarnesses(client!),
  });
  const policyQuery = useQuery({
    queryKey: ["code-permission-policy", machineKey],
    enabled: !!client,
    queryFn: () => getCodePermissionPolicy(client!),
  });

  const workspace = workspacesQuery.data?.find(
    (item) => item.id === workspaceId,
  );
  const harnesses = harnessesQuery.data ?? NO_HARNESSES;
  const ceiling = policyQuery.data?.permission_mode_ceiling;

  const launchableHarnesses = useMemo(
    () =>
      harnesses.filter(
        (harness) =>
          harnessCanStartNow(harness) &&
          permittedPermissionModes(harness.caps, ceiling).length > 0,
      ),
    [ceiling, harnesses],
  );

  useEffect(() => {
    const selected = harnesses.find(
      (harness) => harness.kind === selectedHarnessKind,
    );
    if (
      selected &&
      harnessCanStartNow(selected) &&
      permittedPermissionModes(selected.caps, ceiling).length > 0
    ) {
      return;
    }
    setSelectedHarnessKind(launchableHarnesses[0]?.kind ?? null);
  }, [ceiling, harnesses, launchableHarnesses, selectedHarnessKind]);

  const selectedHarness = harnesses.find(
    (harness) => harness.kind === selectedHarnessKind,
  );
  const permittedModes = selectedHarness
    ? permittedPermissionModes(selectedHarness.caps, ceiling)
    : [];
  const rememberedMode = selectedHarnessKind
    ? modesByHarness[selectedHarnessKind]
    : undefined;
  const selectedMode =
    rememberedMode && permittedModes.includes(rememberedMode)
      ? rememberedMode
      : defaultCreatePermissionMode(permittedModes);

  const modelsQuery = useQuery({
    queryKey: ["code-harness-models", machineKey, selectedHarnessKind],
    enabled: !!client && !!selectedHarnessKind && !!selectedHarness,
    queryFn: () => listCodeHarnessModels(client!, selectedHarnessKind!),
  });
  const models = modelsQuery.data?.models ?? [];
  const rememberedModel = selectedHarnessKind
    ? modelsByHarness[selectedHarnessKind]
    : undefined;
  const selectedModel =
    models.find((model) => model.id === rememberedModel)?.id ??
    models.find((model) => model.default)?.id ??
    models[0]?.id;
  const selectedModeWarning =
    selectedHarness && selectedMode
      ? permissionModeWarning(selectedMode, selectedHarness.caps)
      : null;

  async function startSession() {
    if (
      launchRef.current ||
      !client ||
      !workspace ||
      !selectedHarness ||
      !selectedMode
    ) {
      return;
    }
    launchRef.current = true;
    setLaunching(true);
    setLaunchError(null);
    try {
      const result = await launchCodeSession(
        client,
        workspace.id,
        {
          harness: selectedHarness.kind,
          permission_mode: selectedMode,
          ...(selectedModel ? { model: selectedModel } : {}),
        },
        message,
      );
      if (result.undeliveredDraft) {
        offerDraft(result.session.id, {
          draft: result.undeliveredDraft,
          error: `The session started, but the first message did not send. ${result.sendError ?? "Send it again from this session."}`,
        });
      }
      router.replace({
        pathname: "/session/[id]",
        params: {
          id: result.session.id,
          title: workspace.title || "Session",
          workspace: workspace.title || workspace.branch_name,
        },
      });
    } catch (error) {
      setLaunchError(
        error instanceof Error
          ? error.message
          : "The session could not be started.",
      );
    } finally {
      launchRef.current = false;
      setLaunching(false);
    }
  }

  if (!machine || !client || !workspaceId) {
    return (
      <SafeAreaView className="flex-1 bg-page-background px-5 py-6">
        <ErrorText>Attach a machine before you start a session.</ErrorText>
        <View className="mt-4">
          <Button label="Go back" onPress={() => router.replace("/")} />
        </View>
      </SafeAreaView>
    );
  }

  const optionsLoading =
    workspacesQuery.isLoading ||
    harnessesQuery.isLoading ||
    policyQuery.isLoading;
  const optionsError =
    workspacesQuery.error ?? harnessesQuery.error ?? policyQuery.error;

  return (
    <SafeAreaView className="flex-1 bg-page-background">
      <KeyboardAvoidingView
        className="flex-1"
        behavior={Platform.OS === "ios" ? "padding" : undefined}
        keyboardVerticalOffset={Platform.OS === "ios" ? 74 : 0}
      >
        <ScrollView
          contentContainerClassName="gap-5 px-5 py-6"
          keyboardShouldPersistTaps="handled"
        >
          <Text className="text-2xl font-semibold text-foreground">
            Start session
          </Text>

          {optionsLoading ? <LoadingState label="Loading launch options…" /> : null}
          {optionsError ? (
            <ErrorText>
              {optionsError instanceof Error
                ? optionsError.message
                : "The launch options could not be loaded."}
            </ErrorText>
          ) : null}
          {!optionsLoading && !optionsError && !workspace ? (
            <ErrorText>This workspace is no longer active.</ErrorText>
          ) : null}

          {workspace ? (
            <View className="gap-1 rounded-xl border border-border bg-background p-4">
              <SectionLabel>Workspace</SectionLabel>
              <Text className="text-base font-medium text-foreground">
                {workspace.title || "Untitled workspace"}
              </Text>
              <Text
                className="font-mono text-xs text-muted-foreground"
                numberOfLines={1}
              >
                {workspace.branch_name}
              </Text>
            </View>
          ) : null}

          {!optionsLoading && harnesses.length > 0 ? (
            <View className="gap-2">
              <SectionLabel>Harness</SectionLabel>
              {harnesses.map((harness) => {
                const unavailable = harnessUnavailableReason(harness);
                const policyBlocked =
                  !unavailable &&
                  permittedPermissionModes(harness.caps, ceiling).length === 0;
                return (
                  <ChoiceRow
                    key={harness.kind}
                    label={HARNESS_LABELS[harness.kind]}
                    detail={
                      unavailable ??
                      (policyBlocked
                        ? "Blocked by the machine permission policy."
                        : harness.version
                          ? `Version ${harness.version}`
                          : "Ready")
                    }
                    selected={selectedHarnessKind === harness.kind}
                    disabled={!!unavailable || policyBlocked || launching}
                    onPress={() => {
                      setLaunchError(null);
                      setSelectedHarnessKind(harness.kind);
                    }}
                  />
                );
              })}
              {launchableHarnesses.length === 0 ? (
                <ErrorText>No harness on this machine can start a session.</ErrorText>
              ) : null}
            </View>
          ) : null}
          {!optionsLoading && !optionsError && harnesses.length === 0 ? (
            <ErrorText>No harness is configured on this machine.</ErrorText>
          ) : null}

          {selectedHarness ? (
            <View className="gap-2">
              <SectionLabel>Model</SectionLabel>
              {modelsQuery.isLoading ? (
                <LoadingState label="Loading models…" />
              ) : null}
              {modelsQuery.isError ? (
                <ErrorText>
                  {modelsQuery.error instanceof Error
                    ? modelsQuery.error.message
                    : "The model list could not be loaded."}
                </ErrorText>
              ) : null}
              {!modelsQuery.isLoading && models.length === 0 ? (
                <View className="rounded-xl border border-border bg-background p-4">
                  <Text className="text-sm text-foreground">
                    Use the harness default model.
                  </Text>
                </View>
              ) : null}
              {models.map((model) => (
                <ChoiceRow
                  key={model.id}
                  label={model.label}
                  detail={model.id}
                  meta={model.default ? "Default" : undefined}
                  selected={selectedModel === model.id}
                  disabled={launching}
                  monoDetail
                  onPress={() => {
                    if (!selectedHarnessKind) return;
                    setModelsByHarness((current) => ({
                      ...current,
                      [selectedHarnessKind]: model.id,
                    }));
                  }}
                />
              ))}
            </View>
          ) : null}

          {selectedHarness && permittedModes.length > 0 ? (
            <View className="gap-2">
              <SectionLabel>Permissions</SectionLabel>
              {ceiling ? (
                <Text className="text-xs text-muted-foreground">
                  This machine permits {PERMISSION_MODE_LABELS[ceiling]} or less.
                </Text>
              ) : null}
              {permittedModes.map((mode) => (
                <ChoiceRow
                  key={mode}
                  label={PERMISSION_MODE_LABELS[mode]}
                  detail={PERMISSION_MODE_DESCRIPTIONS[mode]}
                  selected={selectedMode === mode}
                  disabled={launching}
                  onPress={() => {
                    if (!selectedHarnessKind) return;
                    setModesByHarness((current) => ({
                      ...current,
                      [selectedHarnessKind]: mode,
                    }));
                  }}
                />
              ))}
              {selectedModeWarning ? (
                <View className="rounded-lg border border-warning-border bg-warning-background p-3">
                  <Text className="text-sm text-warning-foreground">
                    {selectedModeWarning}
                  </Text>
                </View>
              ) : null}
            </View>
          ) : null}

          {workspace && selectedHarness && selectedMode ? (
            <Field
              label="First message"
              hint="Optional. If sending fails, the created session keeps this draft."
              multiline
              value={message}
              onChangeText={setMessage}
              placeholder="What should the agent work on?"
              editable={!launching}
            />
          ) : null}

          {launchError ? <ErrorText>{launchError}</ErrorText> : null}

          <Button
            label="Start session"
            busy={launching}
            disabled={
              optionsLoading ||
              !!optionsError ||
              !workspace ||
              !selectedHarness ||
              !selectedMode
            }
            onPress={() => void startSession()}
          />
        </ScrollView>
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}

function ChoiceRow({
  label,
  detail,
  meta,
  selected,
  disabled,
  monoDetail = false,
  onPress,
}: {
  label: string;
  detail: string;
  meta?: string;
  selected: boolean;
  disabled: boolean;
  monoDetail?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="radio"
      accessibilityState={{ checked: selected, disabled }}
      disabled={disabled}
      className={`min-h-16 rounded-xl border p-4 disabled:opacity-50 ${
        selected
          ? "border-primary bg-muted"
          : "border-border bg-background"
      }`}
      onPress={onPress}
    >
      <View className="flex-row items-start justify-between gap-3">
        <View className="min-w-0 flex-1 gap-1">
          <Text className="text-sm font-medium text-foreground">{label}</Text>
          <Text
            className={`${monoDetail ? "font-mono " : ""}text-xs text-muted-foreground`}
            numberOfLines={monoDetail ? 1 : undefined}
          >
            {detail}
          </Text>
        </View>
        {selected || meta ? (
          <Text className="text-xs font-medium text-foreground">
            {selected ? "Selected" : meta}
          </Text>
        ) : null}
      </View>
    </Pressable>
  );
}
