// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { HostedSignIn } from "./HostedSignIn";

afterEach(cleanup);

describe("HostedSignIn", () => {
  it("submits a pasted static token and keeps a refusal on the sign-in screen", async () => {
    const onToken = vi
      .fn<(token: string) => Promise<void>>()
      .mockRejectedValueOnce(new Error("refused"))
      .mockResolvedValueOnce();
    const user = userEvent.setup();

    render(
      <HostedSignIn
        reason="no_session"
        machineUrl="https://machine.example.test"
        discovery={{ mode: "static_token" }}
        onToken={onToken}
      />,
    );

    const token = screen.getByLabelText("Token");
    await user.type(token, "  wrong-token  ");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "This machine refused that token.",
    );
    expect(onToken).toHaveBeenLastCalledWith("wrong-token");

    await user.clear(token);
    await user.type(token, "accepted-token");
    await user.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(onToken).toHaveBeenCalledTimes(2));
  });

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
  });
});
