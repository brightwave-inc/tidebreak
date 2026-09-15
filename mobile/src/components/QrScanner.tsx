import { CameraView, useCameraPermissions } from "expo-camera";
import type { BarcodeScanningResult } from "expo-camera";
import { useRef } from "react";
import { Linking, Text, View } from "react-native";
import { Button } from "./Controls";
import { Body } from "./Screen";

/**
 * The camera viewfinder both scanning flows share.
 *
 * It owns the two things neither screen should reinvent: the permission
 * ladder, and the per-frame debounce — `onBarcodeScanned` fires for every
 * frame the camera decodes, so one code held in view would otherwise call back
 * dozens of times a second.
 *
 * The payload is handed to the caller and nothing else. In particular this
 * component never navigates: the standalone flow's payload carries a
 * credential, and routing it would put a token in the router's history.
 */
export function QrScanner({
  permissionPrompt,
  hint,
  onScanned,
}: {
  /** Why this screen wants the camera, shown before permission is granted. */
  permissionPrompt: string;
  /** The line under the viewfinder. */
  hint: string;
  /** Called at most once per debounce window with the raw payload. */
  onScanned: (data: string) => void;
}) {
  const [permission, requestPermission] = useCameraPermissions();
  const lock = useRef(false);

  function handle(result: BarcodeScanningResult) {
    if (lock.current) {
      return;
    }
    lock.current = true;
    setTimeout(() => {
      lock.current = false;
    }, 1500);
    onScanned(result.data);
  }

  if (!permission?.granted) {
    return (
      <View className="gap-3">
        <Body>{permissionPrompt}</Body>
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
      </View>
    );
  }

  return (
    <View className="gap-2">
      <View className="h-96 overflow-hidden rounded-xl border border-border">
        <CameraView
          style={{ flex: 1 }}
          facing="back"
          barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
          onBarcodeScanned={handle}
        />
        {/* Aiming affordance only — the scanner reads the whole frame. */}
        <View
          pointerEvents="none"
          className="absolute inset-0 items-center justify-center"
        >
          <View className="h-56 w-56 rounded-2xl border-2 border-white/90" />
        </View>
      </View>
      <Text className="text-sm text-muted-foreground">{hint}</Text>
    </View>
  );
}
