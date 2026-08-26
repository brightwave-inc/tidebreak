import Constants from "expo-constants";
import * as Linking from "expo-linking";
import * as WebBrowser from "expo-web-browser";
import { useRouter } from "expo-router";
import { useState } from "react";
import { Pressable, Text, TextInput } from "react-native";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import {
  buildAuthorizeRequest,
  exchangeAuthorizationCode,
  fetchGatewayMeta,
  fetchIdentity,
  parseOAuthCallback,
} from "../src/lib/gateway";
import { RESOURCE_CONTROL } from "../src/lib/resource";
import { validatedBaseUrl } from "../src/lib/url";
import { tokenStore } from "../src/session/runtime";
import { useSessionStore } from "../src/session/store";

WebBrowser.maybeCompleteAuthSession();

function redirectUri(): string {
  const extra = Constants.expoConfig?.extra as
    | { oauthRedirectUri?: string }
    | undefined;
  return extra?.oauthRedirectUri ?? "tidebreak://callback";
}

export default function PairScreen() {
  const router = useRouter();
  const setSession = useSessionStore((state) => state.setSession);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function pair() {
    setError(null);
    setBusy(true);
    try {
      const gatewayUrl = validatedBaseUrl(url);
      const meta = await fetchGatewayMeta(gatewayUrl);
      const request = buildAuthorizeRequest(gatewayUrl, redirectUri());
      const result = await WebBrowser.openAuthSessionAsync(
        request.authorizationUrl,
        request.redirectUri,
      );
      if (result.type !== "success" || !("url" in result) || !result.url) {
        throw new Error("Authorization was cancelled.");
      }
      const code = parseOAuthCallback(result.url, request.state);
      const tokens = await exchangeAuthorizationCode(gatewayUrl, {
        code,
        verifier: request.verifier,
        redirectUri: request.redirectUri,
      });
      await tokenStore.replace({
        gatewayUrl,
        refreshToken: tokens.refresh_token,
        installationId: meta.installation_id,
        machinePrefillUrl: meta.tidebreak_machine_url ?? undefined,
        accessTokens: [],
      });
      try {
        const controlToken = await tokenStore.getAccessToken(RESOURCE_CONTROL);
        const identity = await fetchIdentity(gatewayUrl, controlToken);
        await tokenStore.update({ identity });
      } catch {
        // Identity is shown later from a control-scoped mint.
      }
      setSession(tokenStore.snapshot());
      router.replace("/attach");
    } catch (err) {
      setError(err instanceof Error ? err.message : "Pairing failed.");
    } finally {
      setBusy(false);
    }
  }

  return (
    <Screen title="Gateway URL">
      <Body>
        Enter the public base URL of the Model Gateway. The app reads
        unauthenticated installation metadata, then opens the system browser
        for authorization-code + PKCE.
      </Body>
      <TextInput
        autoCapitalize="none"
        autoCorrect={false}
        keyboardType="url"
        placeholder="https://gateway.example"
        placeholderTextColor="#6b7280"
        value={url}
        onChangeText={setUrl}
        className="rounded-lg border border-border bg-background px-3 py-3 text-base text-foreground"
      />
      {error ? <ErrorText>{error}</ErrorText> : null}
      <Pressable
        disabled={busy || url.trim().length === 0}
        className="rounded-lg bg-primary px-4 py-3 disabled:opacity-50"
        onPress={() => void pair()}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          {busy ? "Opening browser…" : "Continue"}
        </Text>
      </Pressable>
      <Text className="text-xs text-muted-foreground">
        Redirect: {Linking.createURL("callback")} / {redirectUri()}
      </Text>
    </Screen>
  );
}
