// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HostedSignIn } from "./HostedSignIn";

afterEach(cleanup);

describe("HostedSignIn", () => {
  /**
   * The token-paste path a standalone machine offers: the machine decides,
   * the screen holds the refusal, and a token that is accepted leaves this
   * screen to boot rather than saying anything more.
   */
  it("submits a pasted token and keeps a refusal on the sign-in screen", async () => {
    const onToken = vi
      .fn<(token: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const user = userEvent.setup();

    render(
      <HostedSignIn
        reason="no_session"
        machineUrl="https://machine.example.test"
        discovery={{ mode: "static_token" }}
        onToken={onToken}
      />,
    );
    // No console to send anyone to, and no OIDC button either.
    expect(screen.queryByRole("link")).toBeNull();

    const token = screen.getByLabelText("Token");
    await user.type(token, "  wrong-token  ");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "This machine refused that token.",
    );
    // The pasted value is trimmed but otherwise untouched.
    expect(onToken).toHaveBeenLastCalledWith("wrong-token");
    expect(token).toHaveValue("  wrong-token  ");

    await user.clear(token);
    await user.type(token, "accepted-token");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    expect(onToken).toHaveBeenLastCalledWith("accepted-token");
    // Accepted: the screen stops saying it was refused and waits for boot.
    expect(screen.queryByRole("alert")).toBeNull();
  });

  /**
   * A session link opened in a fresh browser has to survive the trip through
   * the issuer, so the current hash route rides along as `return_to`.
   */
  it("keeps the current hash route in the OIDC start URL", () => {
    window.history.replaceState({}, "", "/#/c/session-1");
    render(
      <HostedSignIn
        reason="no_session"
        machineUrl="https://machine.example.test"
        discovery={{
          mode: "oidc",
          issuer_name: "login.example.test",
          start_url: "/auth/oidc/start",
        }}
      />,
    );

    expect(
      screen.getByRole("link", { name: /Sign in with login\.example\.test/ }),
    ).toHaveAttribute(
      "href",
      "http://localhost:3000/auth/oidc/start?return_to=%2Fc%2Fsession-1",
    );
    // An OIDC machine takes no pasted token.
    expect(screen.queryByLabelText("Token")).toBeNull();
  });
});
