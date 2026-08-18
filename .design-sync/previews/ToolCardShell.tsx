import { Badge, ToolCardShell } from "tidebreak-desktop-ui";
import { Check, Terminal, X } from "lucide-react";

const icon = <Terminal className="size-3.5 shrink-0" aria-hidden="true" />;

export function Collapsed() {
  return (
    <ToolCardShell
      label="Run a command: Command complete"
      icon={icon}
      title="cargo test -p tidebreak-server"
      titleClassName="font-mono"
      badge={
        <Badge variant="success" className="shrink-0 gap-1">
          <Check className="size-3" aria-hidden="true" />
          Done
        </Badge>
      }
    >
      <div className="p-2 font-mono text-xs whitespace-pre-wrap">
        {"running 214 tests\ntest result: ok. 214 passed; 0 failed; 2 ignored"}
      </div>
    </ToolCardShell>
  );
}

export function Expanded() {
  return (
    <ToolCardShell
      label="Run a command: Command complete"
      icon={icon}
      title="cargo test -p tidebreak-server"
      titleClassName="font-mono"
      defaultExpanded
      badge={
        <Badge variant="success" className="shrink-0 gap-1">
          <Check className="size-3" aria-hidden="true" />
          Done
        </Badge>
      }
    >
      <div className="text-muted-foreground p-2 font-mono text-xs whitespace-pre-wrap">
        {"running 214 tests\n" +
          "test journal::replay_flattens_on_provider_switch ... ok\n" +
          "test turn::cancellation_accounts_usage ... ok\n" +
          "test approvals::grant_ladder_orders_narrowest_first ... ok\n" +
          "\ntest result: ok. 214 passed; 0 failed; 2 ignored; finished in 41.32s"}
      </div>
    </ToolCardShell>
  );
}

export function FailureTint() {
  return (
    <ToolCardShell
      label="Run a command: Tool could not complete"
      icon={icon}
      title="cargo clippy --all-targets -- -D warnings"
      titleClassName="font-mono"
      className="border-destructive"
      badge={
        <Badge variant="outline" className="text-destructive shrink-0 gap-1">
          <X className="size-3" aria-hidden="true" />
          Exit 101
        </Badge>
      }
    >
      <div className="p-2 font-mono text-xs">
        error: unused variable `turn_id`
      </div>
    </ToolCardShell>
  );
}
