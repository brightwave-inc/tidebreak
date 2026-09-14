import { CameraView, useCameraPermissions } from "expo-camera";
import type { BarcodeScanningResult } from "expo-camera";
import { useRouter } from "expo-router";
import { useRef, useState } from "react";
import { Linking, Text, View } from "react-native";
import { Button } from "../src/components/Controls";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import { parsePairingScan } from "../src/lib/provision";

/**
 * Scans the pairing code the gateway console shows.
 *
 * The payload carries a gateway URL and a claimable pairing-session handle,
 * and no credential (`src/lib/provision.ts`). Both are handed back to the pair
 * screen, which probes the gateway and, on the user's confirmation, runs the
 * claim / match-code / console-approval leg.
 */
export default function ScanScreen() {
  const [permission, requestPermission] = useCameraPermissions();
  const [error, setError] = useState<string | null>(null);
  const router = useRouter();
  // The scanner fires per camera frame, so one stray code would otherwise
  // repaint the error state dozens of times a second.
  const lock = useRef(false);

  function onScanned(result: BarcodeScanningResult) {
    if (lock.current) {
      return;
    }
    const scan = parsePairingScan(result.data);
    if (!scan) {
      lock.current = true;
      setError("That code is not a Tidebreak pairing code.");
      setTimeout(() => {
        lock.current = false;
      }, 1500);
      return;
    }
    lock.current = true;
    router.replace({
      pathname: "/pair",
      params: scan.sessionCode
        ? { gateway: scan.gatewayUrl, session: scan.sessionCode }
        : { gateway: scan.gatewayUrl },
    });
  }

  if (!permission?.granted) {
    return (
      <Screen title="Scan to pair">
        <Body>
          Tidebreak needs the camera to read the pairing code shown on your
          gateway console.
        </Body>
        {permission?.canAskAgain === false ? (
          <Button
            label="Open Settings"
            onPress={() => void Linking.openSettings()}
          />
        ) : (
          <Button
            disabled={!permission}
            label="Allow camera access"
            onPress={() => void requestPermission()}
          />
        )}
      </Screen>
    );
  }

  return (
    <Screen title="Scan to pair">
      <View className="h-96 overflow-hidden rounded-xl border border-border">
        <CameraView
          style={{ flex: 1 }}
          facing="back"
          barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
          onBarcodeScanned={onScanned}
        />
        {/* Aiming affordance only — the scanner reads the whole frame. */}
        <View
          pointerEvents="none"
          className="absolute inset-0 items-center justify-center"
        >
          <View className="h-56 w-56 rounded-2xl border-2 border-white/90" />
        </View>
      </View>
      <Text className="text-sm text-muted-foreground">
        Center the pairing code in the square. Nothing in it is a credential —
        you will approve this phone on the console.
      </Text>
      {error ? <ErrorText>{error}</ErrorText> : null}
    </Screen>
  );
}
