import { useQuery } from "@tanstack/react-query";
import { useLocalSearchParams } from "expo-router";
import { useCallback, useMemo, useState } from "react";
import { RefreshControl, ScrollView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import {
  Card,
  DetailRow,
  EmptyState,
  StatTile,
} from "../../src/components/Console";
import { SectionLabel } from "../../src/components/Controls";
import { formatCount } from "../../src/lib/consoleLabels";
import { stamp } from "../../src/lib/consoleTime";
import { formatMicroUsd } from "../../src/lib/consoleTypes";
import type { ConversationView } from "../../src/lib/consoleTypes";
import { HttpError } from "../../src/lib/gatewayClient";
import { conversationQueries } from "../../src/session/consoleQueries";

function payerCost(
  total: number | undefined,
  latestTurn: number | null | undefined,
): string {
  const totalLabel = total === undefined ? "—" : formatMicroUsd(total);
  if (latestTurn == null || latestTurn <= 0) {
    return totalLabel;
  }
  return `${totalLabel} · +${formatMicroUsd(latestTurn)} latest turn`;
}

/**
 * Counts the event window carries rather than the conversation as a whole: the
 * detail read returns at most 500 events, so these are honest about which
 * requests they cover. A null `tool_call_count` is uncounted, not zero, so an
 * all-null window reports nothing instead of a confident zero.
 */
function eventTotals(conversation: ConversationView) {
  let toolCalls = 0;
  let counted = false;
  let failed = 0;
  for (const event of conversation.events) {
    if (event.tool_call_count != null) {
      toolCalls += event.tool_call_count;
      counted = true;
    }
    if (event.terminal_state === "failed") {
      failed += 1;
    }
  }
  return {
    toolCalls: counted ? toolCalls : null,
    failed,
    window: conversation.events_truncated
      ? `in the last ${formatCount(conversation.events.length)} requests`
      : undefined,
  };
}

/**
 * One conversation's headline numbers. A member reads only their own, and a
 * conversation belonging to someone else answers 404 by design — so a 404 here
 * says "not yours or not there", never "no such conversation".
 */
export default function ConversationScreen() {
  const { id } = useLocalSearchParams<{ id: string }>();
  const query = useQuery(conversationQueries.detail(id));
  const conversation = query.data;
  const [refreshing, setRefreshing] = useState(false);
  const onRefresh = useCallback(() => {
    setRefreshing(true);
    void query.refetch().finally(() => setRefreshing(false));
  }, [query]);

  const totals = useMemo(
    () =>
      conversation
        ? eventTotals(conversation)
        : { toolCalls: null, failed: 0, window: undefined },
    [conversation],
  );

  if (query.isError) {
    const notFound =
      query.error instanceof HttpError && query.error.status === 404;
    return (
      <SafeAreaView className="flex-1 bg-page-background">
        {notFound ? (
          <EmptyState
            title="Conversation not found or not yours"
            detail="The gateway answers 404 for a conversation another account owns, so this may exist and simply not be yours to read."
          />
        ) : (
          <EmptyState
            title="Couldn’t load this conversation"
            {...(query.error instanceof Error
              ? { detail: query.error.message }
              : {})}
          />
        )}
      </SafeAreaView>
    );
  }
  if (!conversation) {
    return <SafeAreaView className="flex-1 bg-page-background" />;
  }

  const c = conversation;
  const unpriced = c.priced_request_count < c.inference_requests;
  const hasPayerBreakdown =
    c.subscription_cost_microusd !== undefined ||
    c.provisioned_cost_microusd !== undefined ||
    c.credits_cost_microusd !== undefined ||
    c.last_turn_estimated_cost_microusd !== undefined;
  const provisional =
    c.pending_rate_request_count +
    c.provisional_request_count +
    c.unpriceable_request_count;

  return (
    <SafeAreaView className="flex-1 bg-page-background" edges={["bottom"]}>
      <ScrollView
        contentContainerClassName="gap-3 px-5 py-4"
        refreshControl={
          <RefreshControl refreshing={refreshing} onRefresh={onRefresh} />
        }
      >
        <Text className="text-base font-medium text-foreground">
          {c.title ?? "Untitled conversation"}
        </Text>

        <View className="gap-2">
          <View className="flex-row gap-2">
            <StatTile
              label="Requests"
              value={formatCount(c.inference_requests)}
              {...(unpriced
                ? { detail: "cost is a floor — some unpriced" }
                : {})}
            />
            {/* Billed spend only. Subscription-covered traffic is a different
                payer and this view carries no total for it, so there is nothing
                here to add together. */}
            <StatTile
              label="Est. spend"
              value={formatMicroUsd(c.estimated_cost_microusd)}
              detail="billed only"
            />
          </View>
          <View className="flex-row gap-2">
            <StatTile
              label="Tokens"
              value={`${formatCount(c.input_tokens)} in`}
              detail={`${formatCount(c.output_tokens)} out · ${formatCount(c.cached_input_tokens)} cached`}
            />
            <StatTile
              label="Models"
              value={formatCount(c.model_count)}
              detail={`${formatCount(c.provider_count)} provider${c.provider_count === 1 ? "" : "s"}`}
            />
          </View>
          <View className="flex-row gap-2">
            <StatTile
              label="Tool calls"
              value={totals.toolCalls == null ? "—" : formatCount(totals.toolCalls)}
              {...(totals.toolCalls == null
                ? { detail: "uncounted" }
                : totals.window
                  ? { detail: totals.window }
                  : {})}
            />
            <StatTile
              label="Failed"
              value={formatCount(totals.failed)}
              detail={totals.window ?? "responses that ended failed"}
            />
          </View>
        </View>

        {hasPayerBreakdown ? (
          <Card className="gap-1">
            <SectionLabel>Cost by payer</SectionLabel>
            <DetailRow
              label="Metered"
              value={payerCost(
                c.estimated_cost_microusd,
                c.last_turn_estimated_cost_microusd,
              )}
            />
            <DetailRow
              label="Subscription"
              value={payerCost(
                c.subscription_cost_microusd,
                c.last_turn_subscription_cost_microusd,
              )}
            />
            <DetailRow
              label="Provisioned"
              value={payerCost(
                c.provisioned_cost_microusd,
                c.last_turn_provisioned_cost_microusd,
              )}
            />
            <DetailRow
              label="Credits"
              value={payerCost(
                c.credits_cost_microusd,
                c.last_turn_credits_cost_microusd,
              )}
            />
          </Card>
        ) : null}

        <Card className="gap-1">
          <SectionLabel>Details</SectionLabel>
          <DetailRow label="Harness" value={c.harness} />
          <DetailRow
            label="Account"
            value={c.user_display_name || c.user_email}
          />
          <DetailRow label="First seen" value={stamp(c.first_seen_at)} />
          <DetailRow label="Last activity" value={stamp(c.last_activity_at)} />
          {c.repo_slug ? (
            <DetailRow
              label="Project"
              value={c.repo_ref ? `${c.repo_slug} · ${c.repo_ref}` : c.repo_slug}
            />
          ) : null}
          {c.ephemeral ? (
            <DetailRow label="Resumable" value="No — ephemeral" />
          ) : null}
        </Card>

        {c.project_slug_count > 1 ? (
          <Card>
            <Text className="text-xs text-muted-foreground">
              This conversation has been seen in {c.project_slug_count}{" "}
              projects. The one above is the most recent; earlier requests keep
              the slug they were stamped with, so this conversation’s spend does
              not belong to any single project.
            </Text>
          </Card>
        ) : null}

        {provisional > 0 ? (
          <Card>
            <Text className="text-xs text-muted-foreground">
              {formatCount(c.pending_rate_request_count)} awaiting rates ·{" "}
              {formatCount(c.provisional_request_count)} provisional ·{" "}
              {formatCount(c.unpriceable_request_count)} unpriceable. The
              estimate above is a floor while those stand.
            </Text>
          </Card>
        ) : null}
      </ScrollView>
    </SafeAreaView>
  );
}
