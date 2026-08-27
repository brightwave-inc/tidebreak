export type RailAccountIdentity = {
  title: string;
  detail: string | null;
  githubLogin: string | undefined;
  source: "gateway" | "github" | "local";
};

/**
 * Who the rail account chip names.
 *
 * Tidebreak has no first-party profile. A signed-in model gateway supplies an
 * account hint (usually an email). GitHub's `gh` login, when Delivery already
 * knows it, supplies a face. Otherwise the chip stays empty.
 */
export function railAccountIdentity(input: {
  gateway: { signed_in: boolean; account_hint?: string } | null;
  githubLogin?: string;
}): RailAccountIdentity {
  const githubLogin = trimToUndefined(input.githubLogin);
  const hint =
    input.gateway?.signed_in === true
      ? trimToUndefined(input.gateway.account_hint)
      : undefined;
  if (hint) {
    return {
      title: githubLogin ?? localPart(hint),
      detail: hint === githubLogin ? "Model Gateway" : hint,
      githubLogin,
      source: "gateway",
    };
  }
  if (githubLogin) {
    return {
      title: githubLogin,
      detail: null,
      githubLogin,
      source: "github",
    };
  }
  return {
    title: "Account",
    detail: null,
    githubLogin: undefined,
    source: "local",
  };
}

function trimToUndefined(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  return trimmed ? trimmed : undefined;
}

function localPart(hint: string): string {
  const at = hint.indexOf("@");
  return at > 0 ? hint.slice(0, at) : hint;
}
