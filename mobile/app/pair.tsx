import Constants from "expo-constants";
import * as Linking from "expo-linking";
import * as WebBrowser from "expo-web-browser";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useCallback, useEffect, useRef, useState } from "react";
import { Pressable, Text, TextInput, View } from "react-native";
import { Button } from "../src/components/Controls";
import { Screen, Body, ErrorText } from "../src/components/Screen";
import { attachFailureParams, autoAttach } from "../src/lib/autoAttach";
import {
  buildAuthorizeRequest,
  exchangeAuthorizationCode,
  fetchGatewayMeta,
  fetchIdentity,
  parseOAuthCallback,
} from "../src/lib/gateway";
import {
  awaitPairingApproval,
  claimPairingSession,
  deriveMatchCode,
  pairingErrorMessage,
  pollPairingSession,
} from "../src/lib/pairing";
import { createPkcePair } from "../src/lib/pkce";
import { parsePairingScan } from "../src/lib/provision";
import { RESOURCE_CONTROL } from "../src/lib/resource";
import { grantedScopeFrom } from "../src/lib/scope";
import type { GatewayMeta, TokenResponse } from "../src/lib/types";
import { validatedBaseUrl } from "../src/lib/url";
import { connections } from "../src/session/runtime";

WebBrowser.maybeCompleteAuthSession();

function redirectUri(): string {
  const extra = Constants.expoConfig?.extra as
    | { oauthRedirectUri?: string }
    | undefined;
  return extra?.oauthRedirectUri ?? "tidebreak://callback";
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

type Phase = "idle" | "authorizing" | "approving" | "attaching";

/**
 * Pairing, two ways into the same connection.
 *
 * **Browser sign-in** is the original path: read unauthenticated metadata,
 * open the system browser for authorization-code + PKCE, exchange the code.
 *
 * **QR / console pairing** (mg ADR 0086) is the alternative: scan the code the
 * gateway console shows, claim it with a fresh PKCE challenge, compare a short
 * match code on both screens, and let the person at the console approve this
 * phone. The approval delivers an authorization code that is redeemed through
 * the very same grant — so both paths converge on one `exchangeAuthorizationCode`
 * call and produce an identical persisted connection. Nothing in the QR is a
 * credential, and the console user decides which phone gets in.
 *
 * Everything after the exchange — identity, auto-attach, routing — is shared,
 * because there is no per-path difference left to express by then.
 */
export default function PairScreen() {
  const router = useRouter();
  const params = useLocalSearchParams<{ gateway?: string; session?: string }>();
  const incomingUrl = Linking.useURL();
  const [url, setUrl] = useState("");
  const [phase, setPhase] = useState<Phase>("idle");
  const [error, setError] = useState<string | null>(null);
  const [sessionCode, setSessionCode] = useState<string | null>(null);
  const [matchCode, setMatchCode] = useState<string | null>(null);
  const cancelled = useRef(false);
  const handled = useRef<string | null>(null);
  const busy = phase !== "idle";

  useEffect(() => {
    return () => {
      cancelled.current = true;
    };
  }, []);

  // A scanned code arrives as route params; a provision deep link opened from
  // outside the app arrives as a URL. `parsePairingScan` returns null for the
  // OAuth callback, which shares the scheme, so only provision links land here.
  useEffect(() => {
    const scan =
      typeof params.gateway === "string" && params.gateway.length > 0
        ? {
            gatewayUrl: params.gateway,
            sessionCode:
              typeof params.session === "string" && params.session.length > 0
                ? params.session
                : null,
          }
        : incomingUrl
          ? parsePairingScan(incomingUrl)
          : null;
    if (!scan) {
      return;
    }
    const key = `${scan.gatewayUrl} ${scan.sessionCode ?? ""}`;
    if (handled.current === key) {
      return;
    }
    handled.current = key;
    setUrl(scan.gatewayUrl);
    setSessionCode(scan.sessionCode);
    setError(null);
  }, [params.gateway, params.session, incomingUrl]);

  /**
   * The shared tail of both paths: record the connection, resolve identity,
   * attach the machine the gateway advertises, and route.
   */
  const completeSignIn = useCallback(
    async (gatewayUrl: string, meta: GatewayMeta, tokens: TokenResponse) => {
      const granted = grantedScopeFrom(tokens);
      await connections.addGateway({
        gatewayUrl,
        refreshToken: tokens.refresh_token,
        ...(meta.installation_id
          ? { installationId: meta.installation_id }
          : {}),
        ...(meta.tidebreak_machine_url
          ? { machinePrefillUrl: meta.tidebreak_machine_url }
          : {}),
        // Only what the gateway named as granted. A response that omits the
        // scope leaves it unrecorded, which reads as the baseline authority.
        ...(granted ? { grantedScope: granted } : {}),
      });
      const tokenStore = connections.activeTokens();
      try {
        const controlToken = await tokenStore.getAccessToken(RESOURCE_CONTROL);
        const identity = await fetchIdentity(gatewayUrl, controlToken);
        await connections.updateActive({ identity });
      } catch {
        // Identity is shown later from a control-scoped mint.
      }
      // The gateway named a machine and attach validation refuses anything but
      // that deployment's own, so the confirm screen would be ceremony: run it
      // here. A failure routes to the Attach screen with its error state.
      setPhase("attaching");
      const outcome = await autoAttach(
        {
          gatewayUrl,
          ...(meta.tidebreak_machine_url
            ? { machinePrefillUrl: meta.tidebreak_machine_url }
            : {}),
        },
        { getAccessToken: (resource) => tokenStore.getAccessToken(resource) },
      );
      if (outcome.kind === "attached") {
        await connections.updateActive({ machine: outcome.machine });
        router.replace("/home");
        return;
      }
      router.replace(
        outcome.failure
          ? { pathname: "/attach", params: attachFailureParams(outcome.failure) }
          : "/attach",
      );
    },
    [router],
  );

  async function pairInBrowser() {
    setError(null);
    setPhase("authorizing");
    try {
      const gatewayUrl = validatedBaseUrl(url);
      const meta = await fetchGatewayMeta(gatewayUrl);
      // Meta decides the scope: a gateway refuses a scope it does not
      // advertise outright, which would break sign-in rather than narrow it.
      const request = buildAuthorizeRequest(gatewayUrl, redirectUri(), meta);
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
      await completeSignIn(gatewayUrl, meta, tokens);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Pairing failed.");
    } finally {
      setPhase("idle");
    }
  }

  /**
   * The console-approval leg. The claim carries a challenge generated here and
   * kept here; the QR's session handle alone redeems nothing, which is what
   * makes a photographed code harmless.
   */
  async function pairViaConsole(scannedSession: string) {
    setError(null);
    setPhase("approving");
    cancelled.current = false;
    try {
      const gatewayUrl = validatedBaseUrl(url);
      const meta = await fetchGatewayMeta(gatewayUrl);
      const pkce = createPkcePair();
      const uri = redirectUri();
      await claimPairingSession(gatewayUrl, {
        sessionCode: scannedSession,
        codeChallenge: pkce.challenge,
        redirectUri: uri,
        ...(Constants.deviceName ? { deviceLabel: Constants.deviceName } : {}),
      });
      setMatchCode(deriveMatchCode(pkce.challenge));
      const approval = await awaitPairingApproval({
        poll: () =>
          pollPairingSession(gatewayUrl, scannedSession, pkce.challenge),
        sleep,
        cancelled: () => cancelled.current,
      });
      if (approval.kind === "cancelled") {
        // A claimed session cannot be re-claimed, so resuming needs a fresh
        // code from the console. Browser sign-in stays available.
        setSessionCode(null);
        return;
      }
      const tokens = await exchangeAuthorizationCode(gatewayUrl, {
        code: approval.code,
        verifier: pkce.verifier,
        redirectUri: uri,
      });
      await completeSignIn(gatewayUrl, meta, tokens);
    } catch (err) {
      setError(pairingErrorMessage(err));
      setSessionCode(null);
    } finally {
      setMatchCode(null);
      setPhase("idle");
    }
  }

  if (matchCode) {
    return (
      <Screen title="Approve this phone">
        <Body>
          The gateway console is showing a code. Approve this phone there only
          if it matches.
        </Body>
        <View className="items-center rounded-xl border border-border bg-background py-8">
          <Text className="text-4xl font-semibold tracking-[8px] text-foreground">
            {matchCode}
          </Text>
        </View>
        <Body>
          If the codes differ, deny it on the console — someone else is trying
          to pair.
        </Body>
        <Button
          label="Cancel"
          variant="secondary"
          onPress={() => {
            cancelled.current = true;
          }}
        />
      </Screen>
    );
  }

  return (
    <Screen title="Gateway URL">
      <Body>
        Enter the public base URL of the Model Gateway, or scan the pairing
        code on its console. The app reads unauthenticated installation
        metadata before anything else happens.
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
      {sessionCode ? (
        <View className="rounded-xl border border-info-border bg-info-background p-3 gap-1">
          <Text className="text-sm text-info-foreground">
            Pairing code scanned.
          </Text>
          <Text className="text-xs text-info-foreground">
            Pairing this phone asks the console to approve it — you will compare
            a short code on both screens. No password leaves the console.
          </Text>
        </View>
      ) : null}
      {phase === "attaching" ? (
        <Text className="text-sm text-info-foreground">
          Attaching to the machine this gateway advertises…
        </Text>
      ) : null}
      {error ? <ErrorText>{error}</ErrorText> : null}
      {sessionCode ? (
        <Button
          busy={phase === "approving"}
          disabled={busy || url.trim().length === 0}
          label={phase === "approving" ? "Claiming…" : "Pair this phone"}
          onPress={() => void pairViaConsole(sessionCode)}
        />
      ) : null}
      <Pressable
        disabled={busy || url.trim().length === 0}
        className="rounded-lg bg-primary px-4 py-3 disabled:opacity-50"
        onPress={() => void pairInBrowser()}
      >
        <Text className="text-center text-base font-medium text-primary-foreground">
          {phase === "attaching"
            ? "Attaching…"
            : phase === "authorizing"
              ? "Opening browser…"
              : sessionCode
                ? "Sign in with the browser instead"
                : "Continue"}
        </Text>
      </Pressable>
      <Button
        disabled={busy}
        label="Scan a pairing code"
        variant="secondary"
        onPress={() => router.push("/scan")}
      />
      {/* The alternative onboarding path (#3404). Deliberately one quiet line
          below the gateway flow rather than a second front door: a standalone
          machine is a different way in, not a different product, and the
          gateway path stays the primary one. */}
      <Pressable
        accessibilityRole="link"
        accessibilityLabel="Pair your own Tidebreak instance"
        disabled={busy}
        className="pt-2"
        onPress={() => router.push("/attach-machine")}
      >
        <Text className="text-center text-sm text-muted-foreground underline">
          Pairing your own Tidebreak instance? Tap here
        </Text>
      </Pressable>
    </Screen>
  );
}
