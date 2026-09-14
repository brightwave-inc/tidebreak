/**
 * The console screens' shared furniture.
 *
 * Deliberately built on this repo's own tokens and controls — `StatusPill`,
 * `Button`, `SectionLabel` from `Controls.tsx` — rather than porting
 * Tidewatch's glass chrome and its own palette. The merged app has one visual
 * language, and #3314 owns the pass that unifies it; nothing here should have
 * to be undone first.
 */

import { useState, type ReactNode } from "react";
import { Pressable, Text, View } from "react-native";
import { StatusPill } from "./Controls";
import type { Chip } from "../lib/consoleLabels";
import { formatMicroUsd } from "../lib/consoleTypes";

export function Card({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <View
      className={`rounded-xl border border-border bg-background p-4 ${className}`}
    >
      {children}
    </View>
  );
}

export function ChipPill({ chip }: { chip: Chip }) {
  return <StatusPill tone={chip.tone}>{chip.label}</StatusPill>;
}

/**
 * The one progress bar: a fraction of a bound, tinted by how close it is.
 * `null` renders nothing rather than an empty track — an unknown fraction and
 * a zero one are different facts.
 */
export function Meter({
  fraction,
  warn = false,
}: {
  fraction: number | null;
  warn?: boolean;
}) {
  if (fraction == null) {
    return null;
  }
  const filled = Math.max(0, Math.min(1, fraction));
  const hot = warn || filled >= 0.8;
  return (
    <View className="h-1.5 overflow-hidden rounded-full bg-muted">
      <View
        className={`h-full ${hot ? "bg-warning" : "bg-primary"}`}
        style={{ width: `${Math.round(filled * 100)}%` }}
      />
    </View>
  );
}

/** "spent of ceiling". Renders nothing when the gateway exposes neither. */
export function SpendMeter({
  spendMicroUsd,
  ceilingMicroUsd,
}: {
  spendMicroUsd: number | null | undefined;
  ceilingMicroUsd: number | null | undefined;
}) {
  if (spendMicroUsd == null && ceilingMicroUsd == null) {
    return null;
  }
  const fraction =
    spendMicroUsd != null && ceilingMicroUsd != null && ceilingMicroUsd > 0
      ? spendMicroUsd / ceilingMicroUsd
      : null;
  return (
    <View className="gap-1">
      <Meter fraction={fraction} />
      <View className="flex-row justify-between">
        <Text className="text-xs text-muted-foreground">
          {formatMicroUsd(spendMicroUsd)} spent
        </Text>
        <Text className="text-xs text-muted-foreground">
          {formatMicroUsd(ceilingMicroUsd)} ceiling
        </Text>
      </View>
    </View>
  );
}

/** One headline number, two to a row. */
export function StatTile({
  label,
  value,
  detail,
}: {
  label: string;
  value: string;
  detail?: string;
}) {
  return (
    <View className="flex-1 gap-0.5 rounded-xl border border-border bg-background p-3">
      <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </Text>
      <Text className="text-lg font-semibold text-foreground">{value}</Text>
      {detail ? (
        <Text className="text-xs text-muted-foreground">{detail}</Text>
      ) : null}
    </View>
  );
}

/**
 * One cell of the hub's stat grid — narrower than `StatTile` so three fit a
 * phone row and the whole account picture lands in two rows.
 *
 * `rows` replaces the single headline for the spend card. Billing classes are
 * never summed, so they render as labeled lines of equal weight rather than
 * one total with a footnote that would invite adding them up.
 */
export function StatCell({
  label,
  value,
  sub,
  rows,
}: {
  label: string;
  value?: string;
  sub?: string;
  rows?: { label: string; value: string }[];
}) {
  return (
    <View className="min-h-[92px] flex-1 basis-[30%] gap-0.5 rounded-xl border border-border bg-background p-2.5">
      <Text
        className="text-xs font-medium uppercase tracking-wide text-muted-foreground"
        numberOfLines={2}
      >
        {label}
      </Text>
      {value ? (
        <Text
          className="text-lg font-semibold text-foreground"
          numberOfLines={1}
          adjustsFontSizeToFit
        >
          {value}
        </Text>
      ) : null}
      {rows?.map((row) => (
        <Text
          key={row.label}
          className="pt-0.5 text-sm font-semibold text-foreground"
          numberOfLines={1}
          adjustsFontSizeToFit
        >
          <Text className="text-xs font-normal text-muted-foreground">
            {row.label}
            {"  "}
          </Text>
          {row.value}
        </Text>
      ))}
      {sub ? (
        <Text className="text-xs text-muted-foreground" numberOfLines={2}>
          {sub}
        </Text>
      ) : null}
    </View>
  );
}

/** A tappable row inside a bordered group. */
export function ConsoleRow({
  label,
  detail,
  first = false,
  onPress,
}: {
  label: string;
  detail?: string;
  first?: boolean;
  onPress: () => void;
}) {
  return (
    <Pressable
      accessibilityRole="button"
      accessibilityLabel={label}
      className={`flex-row items-center justify-between gap-3 py-3 ${
        first ? "" : "border-t border-border"
      }`}
      onPress={onPress}
    >
      <Text className="flex-1 text-base text-foreground">{label}</Text>
      <Text className="text-sm text-muted-foreground">
        {detail ? `${detail}  ›` : "›"}
      </Text>
    </Pressable>
  );
}

/** "Label — value", right-aligned, for a facts card. */
export function DetailRow({
  label,
  value,
}: {
  label: string;
  value: string;
}) {
  return (
    <View className="flex-row items-baseline justify-between gap-3 border-t border-border py-2">
      <Text className="text-sm text-muted-foreground">{label}</Text>
      <Text
        className="flex-1 text-right text-sm font-medium text-foreground"
        numberOfLines={2}
      >
        {value}
      </Text>
    </View>
  );
}

export function EmptyState({
  title,
  detail,
}: {
  title: string;
  detail?: string;
}) {
  return (
    <View className="items-center gap-1 py-12">
      <Text className="text-base font-semibold text-foreground">{title}</Text>
      {detail ? (
        <Text className="px-6 text-center text-sm text-muted-foreground">
          {detail}
        </Text>
      ) : null}
    </View>
  );
}

/**
 * A section box that shows the first few rows and expands to the full list —
 * the phone-shaped stand-in for the console's paged tables.
 */
export function ExpandableSection<T>({
  title,
  supporting,
  rows,
  rowKey,
  renderRow,
  previewCount = 3,
  empty,
  footer,
}: {
  title: string;
  supporting?: string;
  rows: T[];
  rowKey: (row: T) => string;
  renderRow: (row: T) => ReactNode;
  previewCount?: number;
  empty: string;
  /** A control that belongs to the section rather than to a single row. */
  footer?: ReactNode;
}) {
  const [expanded, setExpanded] = useState(false);
  const visible = expanded ? rows : rows.slice(0, previewCount);
  return (
    <Card className="gap-2">
      <Text className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {title}
      </Text>
      {supporting ? (
        <Text className="text-xs text-muted-foreground">{supporting}</Text>
      ) : null}
      {rows.length === 0 ? (
        <Text className="py-1 text-sm text-muted-foreground">{empty}</Text>
      ) : (
        <View>
          {visible.map((row, index) => (
            <View
              key={rowKey(row)}
              className={index === 0 ? "py-2" : "border-t border-border py-2"}
            >
              {renderRow(row)}
            </View>
          ))}
        </View>
      )}
      {rows.length > previewCount ? (
        <Pressable
          accessibilityRole="button"
          onPress={() => setExpanded((current) => !current)}
          className="self-start py-1"
        >
          <Text className="text-sm font-medium text-primary">
            {expanded ? "Show less" : `Show all ${rows.length}`}
          </Text>
        </Pressable>
      ) : null}
      {footer}
    </Card>
  );
}
