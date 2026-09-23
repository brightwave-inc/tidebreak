import { useState } from "react";

import { Switch } from "@/components/ui/switch";
import {
  finishedNotificationsEnabled,
  needsYouNotificationsEnabled,
  setFinishedNotificationsEnabled,
  setNeedsYouNotificationsEnabled,
} from "@/NotificationPreferences";
import { SettingsField, SettingsPanel, SettingsSection } from "./primitives";

/**
 * When Tidebreak interrupts you about agent work. Each switch saves the
 * moment it changes, on this device.
 */
export function NotificationsPanel() {
  const [needsYou, setNeedsYou] = useState(needsYouNotificationsEnabled);
  const [finished, setFinished] = useState(finishedNotificationsEnabled);

  return (
    <SettingsPanel
      title="Notifications"
      description="Choose when Tidebreak tells you about agent work. It stays quiet about the conversation you are looking at."
    >
      <SettingsSection title="Desktop notifications">
        <SettingsField
          label="When an agent needs you"
          hint="Tell you when an agent stops for your approval, an answer, or a plan review. While Tidebreak is in the background, you get a desktop notification and the Dock icon bounces until you come back."
        >
          <Switch
            checked={needsYou}
            onCheckedChange={(on) => {
              setNeedsYou(on);
              setNeedsYouNotificationsEnabled(on);
            }}
            aria-label="When an agent needs you"
          />
        </SettingsField>
        <SettingsField
          label="When an agent finishes"
          hint="Tell you when an agent finishes or fails, with why it failed or the first line of its reply."
        >
          <Switch
            checked={finished}
            onCheckedChange={(on) => {
              setFinished(on);
              setFinishedNotificationsEnabled(on);
            }}
            aria-label="When an agent finishes"
          />
        </SettingsField>
      </SettingsSection>
    </SettingsPanel>
  );
}
