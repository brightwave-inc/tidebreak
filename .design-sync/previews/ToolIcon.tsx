import { ToolIcon } from "tidebreak-desktop-ui";

function Cell({ name }: { name: string }) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 8,
        minWidth: 0,
      }}
    >
      <span className="text-muted-foreground">
        <ToolIcon name={name} className="size-4" />
      </span>
      <code style={{ fontSize: 11 }}>{name}</code>
    </div>
  );
}

function Grid({ names }: { names: string[] }) {
  return (
    <div
      style={{
        display: "grid",
        gridTemplateColumns: "repeat(3, minmax(0, 1fr))",
        gap: "10px 24px",
        maxWidth: 760,
      }}
    >
      {names.map((name) => (
        <Cell key={name} name={name} />
      ))}
    </div>
  );
}

export function CoreTools() {
  return (
    <Grid
      names={[
        "search",
        "web_search",
        "web_extract",
        "read_document",
        "read_file",
        "read_tool_result",
        "list_documents",
        "list_dir",
        "write_file",
        "exec",
        "other",
      ]}
    />
  );
}

export function FolderAndAgentTools() {
  return (
    <Grid
      names={[
        "request_folder_access",
        "connect_folder",
        "list_connected_folders",
        "list_folder",
        "read_connected_file",
        "import_connected_file",
        "write_output_to_connected_folder",
        "read_delegated_file",
        "ask_user_questions",
        "exit_plan_mode",
        "update_task_plan",
        "spawn_sandbox_agent",
        "wait_for_agents",
        "create_app",
      ]}
    />
  );
}
