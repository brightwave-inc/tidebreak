import { useRouter } from "expo-router";
import { useState } from "react";
import { QrScanner } from "../src/components/QrScanner";
import { Screen, ErrorText } from "../src/components/Screen";
import { parsePairingScan } from "../src/lib/provision";

/**
 * Scans the pairing code the gateway console shows.
 *
 * The payload carries a gateway URL and a claimable pairing-session handle,
 * and no credential (`src/lib/provision.ts`). Both are handed back to the pair
 * screen, which probes the gateway and, on the user's confirmation, runs the
 * claim / match-code / console-approval leg.
 *
 * Standalone attach does not come through here. Its code *is* a credential, so
 * it is scanned on the attach screen itself and never becomes a route
 * parameter (`app/attach-machine.tsx`, decision 98).
 */
export default function ScanScreen() {
  const [error, setError] = useState<string | null>(null);
  const router = useRouter();

  function onScanned(data: string) {
    const scan = parsePairingScan(data);
    if (!scan) {
      setError("That code is not a Tidebreak pairing code.");
      return;
    }
    router.replace({
      pathname: "/pair",
      params: scan.sessionCode
        ? { gateway: scan.gatewayUrl, session: scan.sessionCode }
        : { gateway: scan.gatewayUrl },
    });
  }

  return (
    <Screen title="Scan to pair">
      <QrScanner
        permissionPrompt="Tidebreak needs the camera to read the pairing code shown on your gateway console."
        hint="Center the pairing code in the square. Nothing in it is a credential — you will approve this phone on the console."
        onScanned={onScanned}
      />
      {error ? <ErrorText>{error}</ErrorText> : null}
    </Screen>
  );
}
