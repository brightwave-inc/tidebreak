import { useEffect, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { useApp } from "./AppContext";
import { ChatRoute } from "./ChatRoute";
import { codeClientGeneration } from "./code/CodeClientGeneration";

/** Older Slack buttons address /c even when another principal owns the session. */
export function LegacyChatRoute({ chatId }: { chatId: string }) {
  const { client } = useApp();
  return (
    <ResolveChatRoute
      key={`${codeClientGeneration(client)}:${chatId}`}
      chatId={chatId}
    />
  );
}
function ResolveChatRoute({ chatId }: { chatId: string }) {
  const { client } = useApp();
  const navigate = useNavigate();
  const [resolved, setResolved] = useState(false);
  useEffect(() => {
    let cancelled = false;
    void client
      .getCodeSession(chatId)
      .then((session) => {
        if (cancelled) return;
        if (session.is_owner === false) {
          void navigate({
            to: "/code/s/$sessionId",
            params: { sessionId: chatId },
            replace: true,
          });
        } else setResolved(true);
      })
      .catch(() => {
        if (!cancelled) setResolved(true);
      });
    return () => {
      cancelled = true;
    };
  }, [client, chatId, navigate]);
  return resolved ? (
    <ChatRoute chatId={chatId} />
  ) : (
    <p role="status" className="text-muted-foreground p-6 text-sm">
      Opening conversation…
    </p>
  );
}
