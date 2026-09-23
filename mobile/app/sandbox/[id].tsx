import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  Alert,
  Keyboard,
  KeyboardAvoidingView,
  Platform,
  Pressable,
  RefreshControl,
  ScrollView,
  Switch,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView, useSafeAreaInsets } from "react-native-safe-area-context";
import { useThemeColors } from "../../src/useThemeColors";
import {
  Card,
  ChipPill,
  ConsoleRow,
  DetailRow,
  EmptyState,
  ExpandableSection,
  SpendMeter,
} from "../../src/components/Console";
import { Button } from "../../src/components/Controls";
import { ConsoleLink } from "../../src/components/Admin";
import { ownerDirectory, ownerLabel } from "../../src/lib/admin";
import {
  continuationGateLabel,
  failureReasonChip,
  phaseChip,
  spacedSlug,
} from "../../src/lib/consoleLabels";
import { clockTime, dayLabel, duration, relative } from "../../src/lib/consoleTime";
import type {
  SandboxEvent,
  SandboxMessage,
  SandboxView,
} from "../../src/lib/consoleTypes";
import { isTerminal } from "../../src/lib/consoleTypes";
import { describeEvent, HIDDEN_EVENT_KINDS } from "../../src/lib/sandboxEvents";
import { grantsRuntimeExecute } from "../../src/lib/scope";
import { administers, canAdminCancel } from "../../src/lib/sections";
import {
  adminQueries,
  meQueries,
  sandboxMutations,
  sandboxQueries,
} from "../../src/session/consoleQueries";
import { useActiveGatewayConnection } from "../../src/session/store";

/** One cell of the fact row: micro-label over a short value. */
function Fact({
  label,
  value,
  hint,
}: {
  label: string;
  value: string;
  hint?: string;
}) {
  return (
    <View className="w-1/2 gap-0.5 pr-3">
      <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </Text>
      <Text className="text-sm font-medium text-foreground">{value}</Text>
      {hint ? (
        <Text className="text-xs text-muted-foreground">{hint}</Text>
      ) : null}
    </View>
  );
}

/**
 * The facts that decide whether steering is worth it: turn budget, whether the
 * run resumes on its own, and the clocks running against it. Every field is
 * optional on an older gateway — an absent fact renders nothing, never a dash.
 */
function FactRow({
  sandbox,
  terminal,
}: {
  sandbox: SandboxView;
  terminal: boolean;
}) {
  const facts: { label: string; value: string; hint?: string }[] = [];
  if (sandbox.turns_used != null) {
    facts.push({
      label: "Turns",
      value:
        sandbox.max_turns != null
          ? `${sandbox.turns_used} of ${sandbox.max_turns}`
          : `${sandbox.turns_used}`,
    });
  }
  if (!terminal && sandbox.may_resume != null) {
    facts.push(
      sandbox.may_resume
        ? { label: "Continuation", value: "Resumes on its own" }
        : {
            label: "Continuation",
            value: "Parked",
            ...(sandbox.continuation_gate
              ? { hint: continuationGateLabel(sandbox.continuation_gate) }
              : {}),
          },
    );
  }
  if (!terminal && sandbox.expires_at) {
    facts.push({ label: "Expires", value: relative(sandbox.expires_at) });
  }
  if (terminal && sandbox.completed_at) {
    facts.push({ label: "Ended", value: relative(sandbox.completed_at) });
  }
  if (sandbox.last_activity_at) {
    facts.push({
      label: "Last activity",
      value: relative(sandbox.last_activity_at),
    });
  }
  if (facts.length === 0) {
    return null;
  }
  return (
    <View className="flex-row flex-wrap gap-y-2 border-t border-border pt-3">
      {facts.map((fact) => (
        <Fact key={fact.label} {...fact} />
      ))}
    </View>
  );
}

/**
 * How this run was configured, folded away by default.
 *
 * None of it changes while the sandbox runs and none of it decides whether to
 * steer, so it loses the fight for the first screen to the phase, the meter and
 * the dock. It is still the first thing anyone wants when a run behaves
 * unexpectedly — which profile, which ref, which ceilings.
 */
function RunDetails({ sandbox }: { sandbox: SandboxView }) {
  const [expanded, setExpanded] = useState(false);
  const rows: { label: string; value: string }[] = [
    { label: "Profile", value: sandbox.profile_name },
    { label: "Started", value: relative(sandbox.created_at) },
  ];
  if (sandbox.repository_url) {
    rows.push({
      label: "Repository",
      value: sandbox.repository_ref
        ? `${sandbox.repository_url} @ ${sandbox.repository_ref}`
        : sandbox.repository_url,
    });
  } else {
    // Not an omission: a sandbox with no repository is a research run, and
    // saying so beats a missing row the reader has to interpret.
    rows.push({ label: "Repository", value: "None — research run" });
  }
  if (sandbox.mode) {
    rows.push({ label: "Mode", value: `${sandbox.mode} mode` });
  }
  // Requested and effective differ when bootstrap could not honour the ask;
  // showing only the effective value would hide that the request was refused.
  if (sandbox.requested_reasoning_effort || sandbox.effective_reasoning_effort) {
    const requested = sandbox.requested_reasoning_effort;
    const effective = sandbox.effective_reasoning_effort;
    rows.push({
      label: "Reasoning effort",
      value:
        requested && effective && requested !== effective
          ? `${spacedSlug(effective)} (asked for ${spacedSlug(requested)})`
          : spacedSlug(effective ?? requested ?? ""),
    });
  }
  if (sandbox.wall_clock_timeout_seconds != null) {
    rows.push({
      label: "Wall-clock ceiling",
      value: duration(sandbox.wall_clock_timeout_seconds),
    });
  }
  if (sandbox.idle_timeout_seconds != null) {
    rows.push({
      label: "Idle ceiling",
      value: duration(sandbox.idle_timeout_seconds),
    });
  }
  if (sandbox.spend_ceiling_microusd != null && sandbox.spend_basis) {
    rows.push({
      label: "Spend measures",
      value: spacedSlug(sandbox.spend_basis),
    });
  }
  return (
    <Card className="gap-1">
      <Pressable
        accessibilityRole="button"
        accessibilityState={{ expanded }}
        onPress={() => setExpanded((current) => !current)}
        className="flex-row items-center justify-between py-0.5"
      >
        <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          Run details
        </Text>
        <Text className="text-sm font-medium text-primary">
          {expanded ? "Hide" : "Show"}
        </Text>
      </Pressable>
      {expanded ? (
        <View>
          {rows.map((row) => (
            <DetailRow key={row.label} {...row} />
          ))}
          <Text className="pt-3 text-xs text-muted-foreground" numberOfLines={1}>
            {sandbox.id}
          </Text>
        </View>
      ) : null}
    </Card>
  );
}

type TimelineEntry = { event: SandboxEvent; heading: string | null };

function TimelineRow({ entry }: { entry: TimelineEntry }) {
  return (
    <View className="gap-1.5">
      {entry.heading ? (
        <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
          {entry.heading}
        </Text>
      ) : null}
      <View className="flex-row gap-3">
        <Text className="w-12 text-xs text-muted-foreground">
          {clockTime(entry.event.created_at)}
        </Text>
        <Text className="flex-1 text-sm text-muted-foreground">
          {describeEvent(entry.event)}
        </Text>
      </View>
    </View>
  );
}

function InboxRow({ message }: { message: SandboxMessage }) {
  return (
    <View className="gap-1">
      <Text
        className={`text-sm ${message.body ? "text-foreground" : "italic text-muted-foreground"}`}
        numberOfLines={3}
      >
        {message.body ?? "Message body not shared with this view"}
      </Text>
      <View className="flex-row flex-wrap items-center gap-2">
        <ChipPill
          chip={{
            label: message.delivered ? "delivered" : "waiting",
            tone: message.delivered ? "neutral" : "live",
          }}
        />
        {message.interrupt ? (
          <ChipPill chip={{ label: "interrupts", tone: "warning" }} />
        ) : null}
        <Text className="text-xs text-muted-foreground">
          #{message.seq} · {relative(message.created_at)}
        </Text>
      </View>
    </View>
  );
}

/**
 * Remounts the surface below when the route's sandbox changes.
 *
 * expo-router reuses this route rather than unmounting it, and every piece of
 * state below belongs to exactly one sandbox: an expanded details card, a
 * shown-ticks toggle, a half-typed steer. The half-typed steer is the reason to
 * care — carried across, it would be sent to the wrong run.
 */
export default function SandboxRoute() {
  const { id } = useLocalSearchParams<{ id: string }>();
  return <SandboxDetail key={id} id={id} />;
}

/**
 * The watch-and-steer surface: now-strip, timeline, steering dock. The dock is
 * pinned to the bottom edge so a correction is always one thumb-reach away;
 * cancel — the only unrecoverable act here — sits at the far end of the scroll
 * instead.
 */
function SandboxDetail({ id }: { id: string }) {
  const colors = useThemeColors();
  const router = useRouter();
  const queryClient = useQueryClient();
  const insets = useSafeAreaInsets();
  const connection = useActiveGatewayConnection();
  const query = useQuery(sandboxQueries.detail(id));
  // The credential's own identity, not the connection record's cached one:
  // what decides ownership of a destructive verb must be asked of the
  // credential rather than read from a display fact.
  const me = useQuery(meQueries.identity());
  const viewerId = me.data?.user_id;
  const detail = query.data;
  const sandbox = detail?.sandbox;

  const [message, setMessage] = useState("");
  const [interrupt, setInterrupt] = useState(false);
  const [composerFocused, setComposerFocused] = useState(false);
  const [receipt, setReceipt] = useState<number | null>(null);
  const [showTicks, setShowTicks] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const composerRef = useRef<TextInput>(null);

  // Known to be someone else's run, judged against the signed-in identity.
  // Deliberately false while that identity has not resolved: hiding the
  // composer from the actual owner is the worse mistake, and a member's read
  // is self-narrowed anyway.
  const notOwn =
    sandbox != null && viewerId != null && sandbox.user_id !== viewerId;
  // Steering and the owner cancel ride `runtime:<slug>`. A session whose
  // consent never carried the runtime scope cannot mint it, so the affordances
  // stay off rather than appearing and failing at the first tap.
  const canSteer = grantsRuntimeExecute(connection?.grantedScope);
  const isAdmin = administers(connection);
  // Stopping somebody else's run needs the role *and* the write scope; the
  // owner verb needs neither.
  const canCancelForeign = canAdminCancel(connection);

  // A name for the owner line, which only an administrator can be reading — a
  // member's every sandbox is their own, and the directory read is refused for
  // one.
  const people = useQuery({ ...adminQueries.people(), enabled: isAdmin });
  const ownerName = useMemo(() => {
    if (!sandbox) {
      return undefined;
    }
    return ownerLabel(
      sandbox.user_id,
      ownerDirectory(people.data?.data ?? []),
      viewerId,
    );
  }, [people.data, sandbox, viewerId]);

  useEffect(() => {
    if (receipt == null) {
      return;
    }
    const timer = setTimeout(() => setReceipt(null), 5000);
    return () => clearTimeout(timer);
  }, [receipt]);

  const send = useMutation({
    mutationFn: () =>
      sandboxMutations.sendMessage(id, message.trim(), interrupt),
    onSuccess: (result) => {
      setMessage("");
      setInterrupt(false);
      setReceipt(result.seq);
      // Collapse the dock: blurring alone is not enough when iOS has already
      // hidden the keyboard without firing onBlur.
      composerRef.current?.blur();
      Keyboard.dismiss();
      setComposerFocused(false);
      void queryClient.invalidateQueries({
        queryKey: sandboxQueries.detail(id).queryKey,
      });
    },
    onError: (error) =>
      Alert.alert(
        "Couldn’t send",
        error instanceof Error ? error.message : "Unknown error",
      ),
  });

  const cancel = useMutation({
    // Someone else's run takes the administrator verb — the runtime verb is
    // owner-scoped and would refuse it. One's own run keeps the owner verb and
    // its termination-intent semantics. The one predicate drives the button,
    // the copy, and the routing, so they cannot disagree.
    mutationFn: () =>
      notOwn ? sandboxMutations.adminCancel(id) : sandboxMutations.cancel(id),
    onSuccess: () =>
      void queryClient.invalidateQueries({
        queryKey: sandboxQueries.detail(id).queryKey,
      }),
    onError: (error) =>
      Alert.alert(
        "Couldn’t cancel",
        error instanceof Error ? error.message : "Unknown error",
      ),
  });

  // Newest first, with each day's first row carrying its own heading. The
  // headings are computed over the whole ordered list, so any prefix the
  // collapsed section shows still opens with the right day.
  const timeline = useMemo(() => {
    const rows = [...(detail?.events ?? [])]
      .filter((event) => showTicks || !HIDDEN_EVENT_KINDS.has(event.kind))
      .sort((a, b) => b.seq - a.seq);
    let previous: string | null = null;
    return rows.map((event) => {
      const day = dayLabel(event.created_at);
      const heading = day === previous ? null : day;
      previous = day;
      return { event, heading };
    });
  }, [detail?.events, showTicks]);

  const hiddenTicks = useMemo(
    () =>
      (detail?.events ?? []).filter((event) =>
        HIDDEN_EVENT_KINDS.has(event.kind),
      ).length,
    [detail?.events],
  );

  const inbox = useMemo(
    () => [...(detail?.inbox ?? [])].sort((a, b) => b.seq - a.seq),
    [detail?.inbox],
  );

  if (query.isError) {
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        <EmptyState
          title="Couldn’t load this sandbox"
          {...(query.error instanceof Error
            ? { detail: query.error.message }
            : {})}
        />
      </SafeAreaView>
    );
  }
  if (!sandbox) {
    return <SafeAreaView className="flex-1 bg-page-background" />;
  }

  const terminal = isTerminal(sandbox.state);
  const waiting = inbox.filter((m) => !m.delivered).length;
  const composerOpen = composerFocused || message.length > 0;
  const showDock = !terminal && !notOwn && canSteer;
  // One's own run offers cancel exactly when this session consented to the
  // runtime scope; somebody else's when the administrator verb is reachable.
  const showCancel =
    !terminal && (notOwn ? canCancelForeign : canSteer);

  function confirmCancel() {
    Alert.alert(
      "Cancel this sandbox?",
      `${
        notOwn ? `This is ${ownerName ?? "another account"}’s run. ` : ""
      }There is no resume and no snapshot — the run’s in-progress work is gone. ${
        sandbox?.repository_url
          ? "Pushed branches and completed output are kept."
          : "Delivered output is kept."
      }`,
      [
        { text: "Keep running", style: "cancel" },
        {
          text: "Cancel sandbox",
          style: "destructive",
          onPress: () => cancel.mutate(),
        },
      ],
    );
  }

  const timelineSupporting = [
    hiddenTicks > 0 && !showTicks
      ? `${hiddenTicks} spend tick${hiddenTicks === 1 ? "" : "s"} hidden.`
      : null,
    detail?.events_truncated
      ? "Earlier events truncated — the full ledger lives on the gateway console."
      : null,
  ]
    .filter(Boolean)
    .join(" ");

  const failure = sandbox.failure_reason
    ? failureReasonChip(sandbox.failure_reason)
    : null;

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <KeyboardAvoidingView
        className="flex-1"
        behavior={Platform.OS === "ios" ? "padding" : undefined}
      >
        <ScrollView
          className="flex-1"
          contentContainerClassName="gap-4 px-5 py-4"
          keyboardShouldPersistTaps="handled"
          refreshControl={
            <RefreshControl
              refreshing={refreshing}
              onRefresh={() => {
                setRefreshing(true);
                void query.refetch().finally(() => setRefreshing(false));
              }}
            />
          }
        >
          <Card className="gap-3">
            <Text className="text-base text-foreground">
              {sandbox.task_prompt}
            </Text>
            {/* Whose run this is, whenever it is not the reader's. A detail
                screen that never says so is how somebody else's run gets read
                as your own. */}
            {ownerName ? (
              <Text className="text-xs text-muted-foreground">
                {ownerName}’s run
              </Text>
            ) : null}
            <View className="flex-row items-center justify-between gap-2">
              <ChipPill chip={phaseChip(sandbox.phase ?? sandbox.state)} />
              <Text className="text-xs text-muted-foreground">
                {sandbox.harness}
                {sandbox.mode ? ` · ${sandbox.mode} mode` : ""}
              </Text>
            </View>
            {sandbox.phase_detail ? (
              <Text className="text-sm text-muted-foreground">
                {sandbox.phase_detail}
              </Text>
            ) : null}
            {sandbox.scheduling_message ? (
              <Text className="text-sm text-warning-foreground">
                {sandbox.scheduling_message}
              </Text>
            ) : null}
            {failure ? <ChipPill chip={failure} /> : null}
            <SpendMeter
              spendMicroUsd={sandbox.spend_microusd}
              ceilingMicroUsd={sandbox.spend_ceiling_microusd}
            />
            {/* An older gateway returns no inbox; fall back to the count. */}
            {detail?.inbox == null && sandbox.pending_messages > 0 ? (
              <Text className="text-xs text-muted-foreground">
                {sandbox.pending_messages} message
                {sandbox.pending_messages === 1 ? "" : "s"} waiting for the next
                turn
              </Text>
            ) : null}
            <FactRow sandbox={sandbox} terminal={terminal} />
          </Card>

          <RunDetails sandbox={sandbox} />

          {/* The conversation read is scoped to whoever ran it, so this tap can
              404 — and nothing on the client knows when. The conversation
              screen names that exact case, so the tap is always offered and the
              destination tells the truth. */}
          {sandbox.spawning_conversation_id ? (
            <View className="rounded-xl border border-border bg-background px-4">
              <ConsoleRow
                label="Spawned from conversation"
                first
                onPress={() =>
                  router.push({
                    pathname: "/conversation/[id]",
                    params: { id: sandbox.spawning_conversation_id as string },
                  })
                }
              />
            </View>
          ) : null}

          {inbox.length > 0 ? (
            <ExpandableSection
              title="Steering inbox"
              {...(waiting > 0
                ? {
                    supporting: `${waiting} waiting — delivery happens on the next turn`,
                  }
                : {})}
              rows={inbox}
              rowKey={(m) => String(m.seq)}
              renderRow={(m) => <InboxRow message={m} />}
              empty="No steering messages yet."
            />
          ) : null}

          <ExpandableSection
            title="Timeline"
            {...(timelineSupporting ? { supporting: timelineSupporting } : {})}
            rows={timeline}
            rowKey={(row) => String(row.event.seq)}
            renderRow={(row) => <TimelineRow entry={row} />}
            previewCount={5}
            empty="No events yet."
            footer={
              hiddenTicks > 0 ? (
                <Pressable
                  accessibilityRole="button"
                  onPress={() => setShowTicks((current) => !current)}
                  className="self-start py-1"
                >
                  <Text className="text-sm font-medium text-primary">
                    {showTicks ? "Hide spend ticks" : "Show spend ticks"}
                  </Text>
                </Pressable>
              ) : null
            }
          />

          {/* The console's sandbox page is administrator-only, so the hand-off
              is offered only to one — anyone else would be redirected away.
              `writesHere` because the cancel button and the steering dock are
              directly below: this session can act on the run here, and the
              stock line would send it away for that. */}
          {isAdmin ? (
            <ConsoleLink webPath={`/sandboxes/${sandbox.id}`} writesHere />
          ) : null}

          {showCancel ? (
            <Button
              label={cancel.isPending ? "Cancelling…" : "Cancel sandbox"}
              variant="destructive"
              onPress={confirmCancel}
              disabled={cancel.isPending}
            />
          ) : null}
        </ScrollView>

        {/* Steering is owner-scoped on the gateway and deliberately not
            admin-overridable, so on someone else's run the composer could only
            be refused. Cancel is different — a write-granted administrator
            reaches the admin verb, and the button stays above — so the note
            names what is still missing rather than leaving a silent gap. */}
        {!terminal && (notOwn || !canSteer) ? (
          <View
            className="border-t border-border bg-background px-5 pt-3"
            style={{ paddingBottom: Math.max(insets.bottom, 12) }}
          >
            <Text className="text-xs text-muted-foreground">
              {notOwn
                ? canCancelForeign
                  ? `Steering is owner-scoped — only ${ownerName ?? "this run’s owner"} can steer it. Cancel is yours as an administrator.`
                  : `Steering and cancel are owner-scoped — only ${ownerName ?? "this run’s owner"} can act on it here. Administrators cancel from the gateway console.`
                : "This pairing was not granted the sandbox verbs. Sign in again to a gateway that offers them to steer or cancel from the phone."}
            </Text>
          </View>
        ) : null}

        {showDock ? (
          <View
            className="gap-2.5 border-t border-border bg-background px-5 pt-3"
            style={{ paddingBottom: Math.max(insets.bottom, 12) }}
          >
            {receipt != null ? (
              <Text className="text-xs text-success-foreground">
                Queued as #{receipt} — delivers on the sandbox’s next turn.
              </Text>
            ) : waiting > 0 ? (
              <Text className="text-xs text-muted-foreground">
                {waiting} message{waiting === 1 ? "" : "s"} waiting for the next
                turn
              </Text>
            ) : null}
            <TextInput
              ref={composerRef}
              className={`rounded-lg border border-border bg-background px-3 py-2.5 text-base text-foreground ${
                composerOpen ? "min-h-20" : "min-h-11"
              }`}
              multiline
              placeholder="Steer this run…"
              placeholderTextColor={colors.mutedForeground}
              accessibilityLabel="Steering message"
              value={message}
              onChangeText={setMessage}
              onFocus={() => setComposerFocused(true)}
              onBlur={() => setComposerFocused(false)}
            />
            {composerOpen ? (
              <>
                <View className="flex-row items-center justify-between">
                  <Text className="flex-1 pr-3 text-sm text-muted-foreground">
                    Stop the turn in flight (leaves the workspace mid-edit)
                  </Text>
                  <Switch value={interrupt} onValueChange={setInterrupt} />
                </View>
                <Button
                  label={
                    send.isPending
                      ? "Sending…"
                      : interrupt
                        ? "Stop this turn and send"
                        : "Send after this turn"
                  }
                  onPress={() => send.mutate()}
                  disabled={send.isPending || message.trim() === ""}
                />
              </>
            ) : null}
          </View>
        ) : null}
      </KeyboardAvoidingView>
    </SafeAreaView>
  );
}
