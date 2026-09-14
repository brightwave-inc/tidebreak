// Generated from the model-gateway admin OpenAPI document. Do not edit.
//
// Source:     GET /api/v1/openapi.json (model-gateway control plane)
// Snapshot:   mobile/schemas/gateway-admin-openapi.json
// Regenerate: pnpm --dir mobile sync-gateway-openapi
// Refresh the snapshot too: pnpm --dir mobile refresh-gateway-openapi

export interface paths {
    "/api/v1/admin/add-on-hosting/allowed-images": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List images approved for managed hosting */
        get: operations["listAllowedAddOnImages"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List add-ons */
        get: operations["listAddOns"];
        put?: never;
        /** Create an add-on */
        post: operations["createAddOn"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Get an add-on */
        get: operations["getAddOn"];
        /** Update an add-on */
        put: operations["updateAddOn"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/clear-previous-secret": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Clear an add-on previous secret */
        post: operations["clearPreviousAddOnSecret"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Enable or disable an add-on */
        post: operations["setAddOnEnabled"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/forge": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Create an add-on's own GitHub App forge
         * @description In one transaction, creates a `git_forge` connected app on https://github.com in `github_app_installation` mode, grants it directly to the add-on's service identity, attaches it to the add-on's MCP endpoint when the add-on's settings name one, and enables it. The App has no identity on GitHub yet: start the manifest flow against the returned app to create it there. Refused with `forge_origin_collision` (409) when the identity already holds another forge app for github.com directly.
         */
        post: operations["provisionAddOnForge"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Read add-on hosting state */
        get: operations["getAddOnHosting"];
        /** Update add-on hosting state */
        put: operations["updateAddOnHosting"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting/environment": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /** Write a managed add-on's environment variables */
        put: operations["updateAddOnHostingEnvironment"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting/install": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Host a first-party add-on on this gateway from its registered preset */
        post: operations["installAddOnHosting"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting/metrics": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a managed add-on's metrics summary
         * @description Fetches `/health/metrics-summary` from the installed workload's in-cluster Service and relays the JSON. Used by the Slack setup page's exit-criteria section.
         */
        get: operations["getAddOnHostingMetrics"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting/pairing": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a managed add-on's pairing report
         * @description Fetches `/health/pairing` from the installed workload's in-cluster Service and relays the outcome: whether it answered, the status it answered with, and the report body when it was JSON. The console cannot reach the add-on itself — its policy admits only the gateway — so this is how the Slack setup page verifies a pairing.
         */
        get: operations["getAddOnHostingPairing"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/hosting/webhook-secret": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Mint or rotate a managed add-on's webhook secret */
        post: operations["mintAddOnWebhookSecret"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/machine-binding": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /** Upsert an add-on machine binding */
        put: operations["upsertAddOnMachineBinding"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/machine-bindings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List add-on machine bindings */
        get: operations["listAddOnMachineBindings"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/probe": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Ask whether an add-on's registered machine URL is reachable right now
         * @description One credential-free GET against each registered machine-binding canonical public URL through the same address guards egress uses (ADR 0069 D6, ADR 0094). The probe answers *reachability*, not authorization: any HTTP response — including a 401 — is a positive liveness signal, and no stored credential is exercised. An add-on with no registered machine URL is refused rather than recorded as failing, so a service-only add-on cannot show a false failure. The outcome is recorded on the add-on's health facts and returned; the detail is a coarse classification, never the origin's own response content.
         */
        post: operations["probeAddOn"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/rotate-secret": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Rotate an add-on client secret */
        post: operations["rotateAddOnSecret"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/{slug}/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Read add-on settings */
        get: operations["getAddOnSettings"];
        /** Update add-on settings */
        put: operations["updateAddOnSettings"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/add-ons/tidebreak/runtime": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** Read Tidebreak execution placement and runtime manifest state */
        get: operations["getTidebreakRuntime"];
        put?: never;
        /**
         * Plan or provision the versioned Tidebreak sandbox runtime
         * @description Uses the same manifest and audited mutations as the setup console. Refuses conflicting resources and verifies the image through the installation trust policy. Repeated requests converge. A partial failure is retryable; existing operator sizing and placement remain intact.
         */
        post: operations["provisionTidebreakRuntime"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/audit-events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read the audit ledger
         * @description Lists recorded administrative actions, newest activity first. This is where a configuration change that has already happened is explained: who changed what, and whether it took effect.
         *
         *     Administrators only. Unlike the other reads here there is no per-caller view of it to narrow to — the ledger records actions taken across the whole installation, and a caller's own rows would be an account of themselves rather than of the gateway.
         *
         *     Narrow before paging. `action` matches by prefix, so `rate_limit.` returns everything that subsystem recorded and `rate_limit.exceeded` only that one action; `outcome` keeps only `succeeded`, `denied`, or `failed` rows; `target_type` and `target_id` name the object an event concerned, and every row carries both back so a lifecycle event can be joined to the thing it happened to. Action prefixes and target identity are indexed, so a subsystem-wide dashboard and a lookup for one object's lifecycle both narrow before paging.
         *
         *     `actor_id` asks the opposite question to `target_id`: it narrows to the actions an account *took*, where `target_id` narrows to the actions taken *against* one. Reading what one administrator changed is `actor_id`; reading what happened to one person's account is `target_type=user&target_id=…`. Pass `actor_type=user` beside `actor_id` — the ledger's actor index leads with the kind, so naming both keeps the lookup an index read.
         *
         *     `target_type` is one of `installation`, `session`, `api_token`, `identity_provider`, `user`, `team`, `model_provider`, `provider_model`, `provider_subscription_binding`, `connected_app`, `mcp_endpoint`, `scim_connector`, `sandbox`, `shared_app`, `connector_session`, `cost_limit_policy`, `rate_limit_policy`, `guardrail_policy`, `guardrail_finding`, `guardrail_engine`, or `add_on`.
         *
         *     Page with `next_cursor`: pass it back as `cursor` until the response returns null. The cursor is a keyset, not an offset, so events recorded while paging do not shift the pages already read. Filters are part of the query, not the cursor — keep them identical across a paging run or the pages describe different ledgers.
         */
        get: operations["listAuditEvents"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/authentication-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read the sign-in policy and who it applies to
         * @description `login_mode` is what interactive sign-in currently accepts.
         *
         *     `passwordless` is the count the `sso_only_accounts` refusal is decided against: active accounts holding no password at all. Non-zero means withdrawing the last enabled verified provider will be refused. `local_only` and `sso` describe the population and do not predict it — an account can hold both a password and a provider link, so it is counted in `sso` and is not stranded by withdrawing the provider. `sso` being non-zero says nothing about whether the withdrawal will be refused.
         *
         *     The other refusal, `sso_enforced`, is read straight off `login_mode`: a mode requiring SSO refuses withdrawing the last enabled verified provider whatever the counts say.
         *
         *     Read-only here. Changing the policy to enforce SSO is checked against the acting *session*'s own provider link — the administrator making the change has to be signed in through SSO themselves, so that enforcing it cannot lock out the person doing it. A token has no such link to check, so the change is made from the identity page.
         */
        get: operations["getAuthenticationPolicy"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-app-connections": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List my connected-app connections
         * @description Returns the same per-account connection facts used by the account surface: whether the caller has connected OAuth, whether that grant needs reauthorization, and whether a personal static credential is stored. The caller is taken from authentication; there is no account selector and administrators do not receive other users' status. No credential or upstream identity is returned.
         */
        get: operations["listMyConnectedAppConnections"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-app-connections/{app_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Disconnect me from a connected app
         * @description Deletes only the calling account's credential binding for this app. OAuth access and refresh tokens are removed together; a static personal credential is removed by the same operation. Installation credentials and every other account are untouched. The operation is idempotent and returns 204 when the caller was already disconnected, so it does not reveal whether an app outside the caller's account surface exists.
         */
        delete: operations["disconnectMyConnectedApp"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List connected apps
         * @description Every connected app with the terms it is reachable on: whether it is enabled, which model routes may invoke it, which agent interfaces expose it, and whether a caller's own credential may stand in for the installation's.
         *
         *     `credential_configured` reports whether a credential is present, not what it is — nothing here discloses a stored secret, and there is no operation in this document that reads one back. For a GitHub App-backed git forge, `git_forge_github_app_last_minted_at` is the per-process liveness signal from the in-memory token cache; null means only that this replica has no successful mint to report. A `github_app` forge with a stored OAuth client also reports `git_forge_has_intrinsic_rest_channel` and `git_forge_intrinsic_rest_operation_count` — the kind-intrinsic PR catalog, not a `rest_api` projection. `rest_tool_slug` and `rest_operation_count` stay the REST-app discriminator.
         */
        get: operations["listConnectedApps"];
        put?: never;
        /**
         * Register an outside system the gateway will call
         * @description Creates an app disabled, holding the credential the gateway will present. Disabled is not a defect to correct: an app that became reachable the moment it was created would be reachable before its model policy, its team grants, or its agent access had been decided. `setConnectedAppEnabled` turns it on once they have.
         *
         *     Every member of `credential` is write-only. Nothing in this document reads stored credential material back; `credential_configured` on the app view reports only that something is stored, and there is no operation to rotate a credential — an app whose secret changed is replaced.
         *
         *     Which members of the body apply is decided by `kind`. `datadog` and `vercel` authenticate with a shared secret, while `sentry`, `linear`, `figma`, `slack`, `atlassian`, and `pagerduty` use each caller's own delegated grant, so `auth_method` is fixed for those native kinds and anything sent for it is ignored. `datadog` needs `credential.scheme` because it is what tells its two recipes apart. `sentry` defaults its endpoint, OAuth URLs, and scopes. `figma` defaults its endpoint, OAuth URLs, and scopes the same way, and authenticates at Figma's token endpoint with HTTP Basic. `pagerduty` defaults the same three and authenticates with `client_secret_post`; its endpoint may only be one of PagerDuty's two regional API origins, and its OAuth URLs are pinned to the one global identity service both regions use, so sending anything else for them is refused rather than stored. `vercel` defaults its endpoint to `https://api.vercel.com`, accepts no other origin, and injects `credential.shared_secret` as a bearer token. `datadog`, `sentry`, `figma`, `pagerduty`, and `vercel` carry OpenAPI documents that ship with the gateway. `linear` and `atlassian` fix their hosted MCP endpoints, perform OAuth discovery and dynamic client registration, and carry the compiled MCP transport catalog, so `rest_api.openapi_document` is read only for `rest_api`. `slack` fixes the hosted MCP endpoint and its authorize and token endpoints, and carries the same compiled catalog, but Slack offers no dynamic registration: send the client ID in `oauth` and the secret in `credential.oauth_client_secret`, both required, and leave the OAuth URLs out — a host other than Slack's own is refused. `gitlab` derives its OAuth endpoints from `endpoint_url` — `<base>/oauth/authorize` and `<base>/oauth/token`, keeping any path prefix a relative-URL install carries — and takes the client id and secret an administrator created on the instance. It declares the streaming git channel only, and its push credential is each caller's own grant, presented as `oauth2:<token>`.
         *
         *     The built-in app is refused with `built_in_app`. The gateway provisions that one itself at startup, with an origin and a document no request could supply. A reused name is `name_taken`, and a tool slug another governed REST app already exposes is `tool_slug_taken` — a slug is what an agent addresses a tool by, so two apps cannot share one.
         *
         *     `credential_required` means the app authenticates with a stored credential and none was sent; `conflicting_credentials` means a secret was sent that this app would not use — either a second recipe alongside the one it takes, or a lone secret an app that authenticates with nothing has no use for. It names the field to remove. Nothing sent here is ever silently dropped, so a `201` means every secret in the request was stored. `invalid_openapi` means the document could not be projected into callable tools.
         *
         *     `invalid_configuration` reports that a value, or its relation to the ones beside it, does not describe a usable app. It names the field to change, but the checks behind it are relations between fields — a credential placement the authentication method forbids, a delegated-OAuth app missing its client — so the named field is where to start rather than the whole of what is wrong.
         *
         *     A `git_forge` app carries `git_forge` instead of `rest_api`: git's protocol endpoints are declared in code, so there is no document to supply, and its ceilings bound a streamed packfile rather than a buffered body. A bound outside its range is refused rather than narrowed to fit. A basic-placement forge names its login convention in `credential.login` — `x_access_token`, `oauth2`, or `bot_user` — and supplies `credential.username` only for `bot_user`.
         *
         *     Excluded from self-administration. This operation can mint a `git_forge` app, which is a credentialed path that pushes code to a forge; a model editing the gateway through the built-in app must not be able to create the transport its own sandboxes then push through.
         */
        post: operations["createConnectedApp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete a connected app
         * @description Removes the app and the credential it holds. Refused with `app_in_use` while an MCP endpoint or team grant still references it: detaching those first is what makes the loss of access visible on the endpoint and the team, where somebody is watching for it.
         *
         *     The built-in app is refused with `built_in_app` and always will be — startup reconciliation recreates it, so a delete would report success and not stick. Disable it instead.
         */
        delete: operations["deleteConnectedApp"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/access-policies": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List an app's access-policy statements
         * @description Every stored statement constraining this app's operations, widest scope first (ADR 0067). The channel's catalog is the only allow plane; a `deny` subtracts from it and is never overridden, and an `allow` pins the principals its scope covers to an explicit subset without granting anything the catalog lacks. Seeded default postures — the GitHub GraphQL deny — appear here like any other row, which is the point: the default is a statement an administrator can see and delete, not an absence to intuit.
         */
        get: operations["listConnectedAppAccessPolicies"];
        put?: never;
        /**
         * Constrain an app's operations at a scope
         * @description Adds one statement (ADR 0067). Statements are immutable: correcting one is a delete and a create, so the audit ledger records both halves. The operation must be in the app's ceiling — refusing an operation that cannot match keeps the rendered policy honest — and `*` is accepted only on a deny, because an allow of everything constrains nothing. A deny at any scope always wins; an allow restricts the covered principals to the union of the allow-sets at its scope kind. On an app whose upstream speaks MCP, a `tool:<name>` id names one tool and is accepted outside the ceiling (ADR 0076): the vendor owns the tool set, so only the id's shape is checked. A tool no statement mentions inherits the channel's verdict — reachable unless an allow-set at some scope enumerates tools without it. On a git forge, `query:<field>` and `mutation:<field>` name one GraphQL operation the same way, refining the forge's GraphQL request operation.
         */
        post: operations["createConnectedAppAccessPolicy"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/access-policies/{policy_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete one access-policy statement
         * @description The only widening act in the model (ADR 0067), and an audited one. Deleting a deny restores what the ceiling and the remaining statements admit — it cannot widen a principal past another scope's statement, which is what makes deleting the seeded GitHub GraphQL deny safe to reason about: fleets whose profile carries its own deny keep refusing.
         */
        delete: operations["deleteConnectedAppAccessPolicy"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/agent-access": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Change which agent interfaces expose an app
         * @description `proxy_api` exposes the generated structured MCP tool, which admits only the operations the app's `OpenAPI` document declares. `governed_shell` exposes the app to the shell runtime, where an agent composes its own requests against the same policy. `both` exposes each.
         *
         *     Neither interface widens what the app may do: the request is authorized per call either way, and the difference is how the agent expresses it. Under ADR 0024 an interface change creates no trust-tier conflict, so unlike `setConnectedAppModelPolicy` this one has nothing to resolve.
         *
         *     Refused with `app_kind_unsupported` for an app with no buffered REST channel. GitHub and GitLab forges carry a compiled catalog instead of an administrator-supplied `OpenAPI` document, so this setting applies to them too. It never controls a sandbox's repository transport.
         */
        put: operations["setConnectedAppAgentAccess"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/audience": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set who may reach a built-in app
         * @description Audience exists for apps the gateway provisions itself, which have no team grant to carry reachability. Any other app is refused with `app_kind_unsupported`: allowing it there would create a second, quieter grant path beside the team grants an administrator reviews.
         *
         *     This gates reachability only. Every request still carries the calling user's own authority, so widening the audience cannot widen what any individual caller is permitted to do.
         */
        put: operations["setConnectedAppAudience"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/ca-trust": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read what one app's egress trusts beyond the platform roots
         * @description Returns the stored PEM together with each certificate parsed out of it: subject, issuer, validity window, and whether it carries the CA basic constraint. The PEM is returned in full, unlike every credential on this app — a CA certificate is public material an upstream hands to every client that connects to it.
         *
         *     An expired certificate and one that is not a CA are both reported rather than refused. Staging a rotation by pasting next year's root early is legitimate, and so is pinning a self-managed host's own self-signed certificate.
         */
        get: operations["getConnectedAppCaTrust"];
        /**
         * Trust extra CA certificates for one app's egress
         * @description Supplies the certificate authority an on-prem upstream's TLS certificate was issued by, so a GitHub Enterprise Server or a self-managed GitLab behind a corporate CA can be reached at all. The roots are *added* to the platform trust store, never substituted for it, and there is no operation anywhere in this document that accepts an invalid certificate.
         *
         *     One bundle covers every call the app makes: the buffered REST path, the git stream, the health probe, the OAuth token exchange, and a GitHub App mint. It does not decide whether the upstream's *address* may be private — `allow_private_networks` on the connection does, and an on-prem host usually needs both.
         *
         *     The PEM is parsed before it is stored. A bundle carrying private-key material is refused as `ca_trust_private_key`; one that parses as no certificate at all is `ca_trust_empty`; a block that is not valid X.509 is `ca_trust_malformed`; and the size and count ceilings refuse as `ca_trust_too_large` and `ca_trust_too_many`. Sending a blank bundle clears it, exactly as `deleteConnectedAppCaTrust` does.
         */
        put: operations["setConnectedAppCaTrust"];
        post?: never;
        /**
         * Stop trusting an app's extra CA certificates
         * @description Clears the bundle so the app's egress verifies against the platform trust store alone. Idempotent: an app with no bundle configured is already in the state this leaves it in.
         *
         *     This is a narrowing, so it can break a working integration — an upstream whose certificate only verified because of these roots starts failing TLS immediately. It never widens anything.
         */
        delete: operations["deleteConnectedAppCaTrust"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/effective-access": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Judge every ceiling operation for a chosen principal
         * @description Runs the boundary's own composition — union of allow-sets within a scope kind, intersection across kinds, deny unconditionally strongest — over every operation in the app's ceiling, for the principal the query names. Rendered server-side so what an administrator reads and what the boundary enforces cannot drift apart. On an MCP app the judged set also carries every `tool:` id this principal's statements name (ADR 0076); the tool set itself is the vendor's and is not enumerable here. Refused for an app whose ceiling is not enumerable here.
         */
        get: operations["computeConnectedAppEffectiveAccess"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Enable or disable a connected app
         * @description Disabling stops the app being invoked without removing its credential, its endpoint assignments, or the grants that reach it. It is the reversible way to stop an app, and the only way to stop the built-in one — `deleteConnectedApp` refuses that app because startup reconciliation would recreate it.
         */
        put: operations["setConnectedAppEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/git-forge/github-app": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Describe the GitHub App a git forge mints installation tokens from
         * @description Reports what the forge knows about its GitHub App, and — when it holds a signing key — asks the forge itself for the app's slug so the install page can be linked.
         *
         *     No credential material is returned. The slug, name, and page URL are what GitHub publishes about the app to anyone; the private key that signs the call is never read back by any operation in this document. `last_minted_at` reflects this gateway process only: minted tokens are cached in memory and never stored, so a replica that has not minted since it started reports nothing rather than claiming the installation is broken.
         *
         *     When the forge names an installation and holds a signing key, this also asks the forge which permissions that installation has accepted, and reports the ones this gateway's manifest asks for that the grant does not cover. A non-empty `unmet_permissions` is the expected state after a release widens the manifest — editing a manifest cannot grow an App that already exists, so the grant only moves when an owner re-approves — and it is not a fault in the forge. `permissions_observed` distinguishes "asked, and the installation is current" from "could not ask"; the two must not be read as the same answer. Asked live on every load rather than remembered, because an owner accepting permissions on GitHub leaves no trace here and a stored answer would keep reporting drift that was already resolved.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in `github_app` credential mode.
         *
         *     Excluded from self-administration: it exercises the forge's own minting authority, which is configuration an administrator wrote and not something a model administering the gateway may reach.
         */
        get: operations["getGitForgeGithubApp"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/git-forge/github-app/installation": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Choose the installation a git forge mints tokens for
         * @description Records which approved installation of the forge's GitHub App the gateway mints installation tokens against. Until one is chosen the forge has no credential and its git egress is denied.
         *
         *     This is the only part of a forge's credential that is editable after creation, and deliberately so: the installation is the one part that cannot exist when the app is created, because a human has to approve it first. The signing key is not editable, on the same terms as every other stored secret — an app whose credential changed is replaced.
         *
         *     The identifier is verified against the GitHub App's own installation list before it is stored, so a value that is stale or mistyped is refused with `invalid_configuration` here rather than accepted and discovered hours later, when a clone denies with `forge_credential_mint_failed` and points at the credential instead of at the configuration.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in `github_app` credential mode holding a GitHub App identity — an installation cannot belong to an app that does not exist yet.
         *
         *     This is the operation that *changes* a binding. The forge's own setup redirect may only fill an empty one, because it is a GET a third party navigates a browser to and carries no CSRF defence.
         *
         *     Excluded from self-administration: it is a write to a forge's push credential, which a model administering the gateway must not reach.
         */
        put: operations["setGitForgeGithubAppInstallation"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/git-forge/github-app/installations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List the installations a git forge's GitHub App has
         * @description Asks the forge which accounts have approved this app, so an administrator can choose the one to mint tokens for. Every entry is an account login and an installation identifier the forge publishes; nothing here is credential material.
         *
         *     An empty list is the ordinary answer before anybody has approved the install, not an error.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in `github_app` credential mode, and with `invalid_configuration` for one that holds no signing key yet — there is nothing to authenticate the question with.
         *
         *     Excluded from self-administration for the reason getGitForgeGithubApp is.
         */
        get: operations["listGitForgeGithubAppInstallations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/git-forge/installation-repositories": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List repositories a git forge's installation covers
         * @description Asks the forge which repositories the chosen installation can see, so an administrator can pick one and inspect sandbox WIP refs on it. The first page only, matching the other installation list surfaces. Nothing here is credential material.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in GitHub App credential mode, and with `invalid_configuration` when the forge has no signing key or installation yet.
         *
         *     Excluded from self-administration: it mints an installation token.
         */
        get: operations["listGitForgeInstallationRepositories"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/git-forge/wip-refs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List sandbox WIP refs on one repository
         * @description Lists `mg-wip/*` branches on one repository the forge installation covers. This is operator hygiene for leftover checkpoint refs (#1395), not an agent tool: sandboxes must not garbage-collect their own memory.
         *
         *     `repository` is `owner/repo` as GitHub names it. Refs outside `mg-wip/` are dropped even if the forge returned them.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in GitHub App credential mode.
         *
         *     Excluded from self-administration: it mints an installation token and reads a forge the model did not pick.
         */
        get: operations["listGitForgeWipRefs"];
        put?: never;
        post?: never;
        /**
         * Delete one sandbox WIP ref
         * @description Deletes one `mg-wip/*` branch on a repository the forge installation covers. Names outside that prefix are refused here, before the forge is called, so this cannot be aimed at `main`.
         *
         *     Operator hygiene (#1395), not an agent tool.
         *
         *     Refused with `app_kind_unsupported` for any app that is not a live `git_forge` in GitHub App credential mode.
         *
         *     Excluded from self-administration: it writes to a forge using the installation credential.
         */
        delete: operations["deleteGitForgeWipRef"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/model-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Replace which model routes may invoke an app
         * @description Replaces the policy wholesale rather than amending it: send the routes that should be allowed, not the ones to add. `all` means every route the caller already holds a grant for, and requires `model_ids` to be empty — sending both is refused rather than silently ignoring one, because the two readings differ by exactly the access in question.
         *
         *     The app this gateway provisions for itself is in scope here deliberately, unlike `deleteConnectedApp`: this is how its allowed routes are administered, and there is no other operation that sets them. Restricting it does not exempt it from the trust-tier rule below — a built-in app on a Direct endpoint conflicts either way.
         *
         *     Restricting an app can strand an MCP endpoint that exposes it on the Direct trust tier. `direct_endpoint_resolution` decides in advance: the default `reject` refuses with `direct_endpoint_conflict` and changes nothing, `switch_to_gateway_attested` keeps the assignments and moves those endpoints, and `remove_assignments` keeps the endpoints and drops the app from them. Whichever is chosen, the endpoints it touched come back in the response and each one gets its own audit row against the endpoint — an administrator asking why an endpoint changed tier reads the endpoint's history, and a row filed only against the app would not appear there.
         *
         *     Unknown or repeated route IDs are refused as `unknown_model` and `duplicate_model`; a repeat is a caller building a list wrong, and accepting it would make the stored policy disagree with what was sent.
         */
        put: operations["setConnectedAppModelPolicy"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/name": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Rename a connected app
         * @description Changes only the administrator-facing name. Everything the app is — its origin, its stored credential, its tool slug, its grants, and the recorded calls made through it — is untouched, which is the point: without this, correcting a typo in a connected git forge's name means deleting it, which destroys the GitHub App signing key it holds and orphans the App on the forge.
         *
         *     Refused with `name_taken` when another **live** app already carries the name; a deleted app holds none, so a name freed by a delete can be taken here. The built-in app is refused with `built_in_app`: startup reconciliation rewrites its name from the running binary, so a rename would report success and be gone by the next restart.
         *
         *     Both names are written to the audit ledger, since the row only ever holds the current one.
         */
        put: operations["setConnectedAppName"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/oauth-scopes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set a connected app's OAuth scopes
         * @description Changes only the non-secret `scope` value used on future authorization redirects. Existing connections keep their recorded `granted_scopes`, and no client credential is read or replaced. Blank removes the `scope` query parameter entirely.
         *
         *     The authorization URL, token URL, and client ID remain immutable: although they are not secrets, they identify the issuer/client relationship under which live user grants were minted. Re-pointing those fields in place would make the row describe a different OAuth client while retaining the old grants. Replace the app when that identity changes.
         *
         *     Refused with `app_kind_unsupported` when the app has no per-user OAuth Connect flow. The built-in app is unsupported for the same reason.
         */
        put: operations["setConnectedAppOauthScopes"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/permanently": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Permanently remove a deleted connected app
         * @description Removes the row `deleteConnectedApp` left behind, together with its projection, its stored credential, the per-user connections under it, its model allowlist, and any shared-app bindings pointing at it. This cannot be undone: a git forge's GitHub App signing key is destroyed with it and cannot be obtained from the forge again.
         *
         *     Only a deleted app can be purged; a live one is refused with `app_not_deleted`. The two steps are deliberate — this destroys configuration that audit entries point at, and that should not be one action away from an app somebody is using.
         *
         *     Refused with `app_history_retained` when recorded tool calls, outbound requests, runtime executions, or sandboxes still reference the app. That history is kept on purpose and nothing removes it, so those apps stay as deleted rows; the refusal names what holds the app and how much. Refused with `app_in_use` when live configuration elsewhere — an MCP endpoint assignment — still points at it, which is detachable.
         */
        delete: operations["purgeConnectedApp"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/personal-credential-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set whether callers may use their own credential
         * @description `installation_only` sends every call through the credential the installation holds. `personal_preferred` uses the caller's own credential when they have attached one and falls back to the installation's otherwise — so upstream sees the individual rather than the gateway, and the upstream's own permissions apply per person.
         *
         *     The choice is recorded in the audit ledger under the policy that was set, not under the act of setting it, so the ledger reads as a history of what the installation permitted.
         */
        put: operations["setConnectedAppPersonalCredentialPolicy"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/{app_id}/probe": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Ask whether a connected app's origin is reachable right now
         * @description One credential-free GET against the app's endpoint URL through the same address guards egress uses (ADR 0069 D3/D6). The probe answers *reachability*, not authorization: any HTTP response — a 401 from an OAuth-protected server, or the 405 an MCP endpoint returns to a bare GET — is a positive liveness signal, and no stored credential is exercised out of band. The outcome is recorded on the app's health facts and returned; the detail is a coarse classification (`http_405` for that MCP case), never the origin's own response content.
         */
        post: operations["probeConnectedApp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connected-apps/deleted": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List deleted connected apps
         * @description Deleting an app is a soft delete — the row survives so that the tool calls, outbound requests, and runtime executions recorded against it keep resolving to something. `listConnectedApps` therefore cannot show these rows, and without this operation nothing can.
         *
         *     A deleted app holds neither its name nor its tool slug: both are free for a new app the moment it is deleted. What it still holds is its configuration, and `purgeConnectedApp` is what removes that.
         *
         *     Each row reports whether that purge would succeed. `purgeable` is true when nothing references the app; `held_by` counts what does, split the way the purge refusal splits it — recorded history (tool calls, outbound requests, runtime executions, sandbox records) is kept for as long as the record exists and nothing removes it, so an app it references stays a deleted row, while an MCP endpoint assignment is configuration an administrator can detach and retry. The counts are a snapshot; the purge re-counts under a lock and remains the authority.
         */
        get: operations["listDeletedConnectedApps"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connector-access-profiles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create a connector access profile */
        post: operations["createConnectorAccessProfile"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connector-connections": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Create a logical connector connection */
        post: operations["createConnectorConnection"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connector-grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Grant a user a connector access profile */
        post: operations["createConnectorGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connector-sites": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List connector sites */
        get: operations["listConnectorSites"];
        put?: never;
        /** Create a connector site */
        post: operations["createConnectorSite"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connector-sites/{site_id}/snapshots": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Sign and publish a site snapshot */
        post: operations["publishConnectorSnapshot"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connectors": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Register a connector instance */
        post: operations["createConnector"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/connectors/{connector_id}/disable": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Disable a connector instance */
        post: operations["disableConnector"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/conversations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List conversations, most recently active first
         * @description Finds the conversation worth reading. Each row carries the same aggregates `getConversation` reports — token counts, the `cost_state` breakdown, estimated metered, subscription, provisioned-capacity, and prepaid-credit cost, model and provider counts, tool and app-request counts — without its event list or its latency distribution.
         *
         *     Administrators list the installation; every other caller lists only their own conversations. There is no `user_id` filter: the scope is the caller's authority, so an administrator narrowing to one account reads that account's usage through `listUsage` instead.
         *
         *     Costs are integer micro-US-dollars — 1 000 000 to the dollar. `estimated_cost_microusd` covers metered spend only and is a floor rather than a total wherever `priced_request_count` is below `inference_requests`; `getCostStateSummary` says why. `subscription_cost_microusd` is notional — what the traffic would have cost had a user's own subscription not absorbed it. `provisioned_cost_microusd` is the Standard-rate equivalent of traffic absorbed by provider-provisioned capacity. `credits_cost_microusd` is prepaid-credit draw-down.
         *
         *     `repo_slug`, `repo_ref`, and `workspace_name` are the latest observed project context (ADR 0059) and are display handles, not spend keys: a conversation resumed in another clone reports the newest slug while its earlier events keep the one they were stamped with, so summing these rows by `repo_slug` misattributes cost. `project_slug_count` above one says the handle is one of several, and `getConversation` lists the whole set.
         *
         *     Narrow to an activity window with an inclusive `since` and an exclusive `until`; the window bounds the *events* counted into each row, so a conversation whose traffic falls entirely outside it does not appear. Equality filters (`harness`, `repo_slug`) and free-text `q` (title, repo, workspace, owner, or id) bind against the conversation row before aggregates run, so a narrow page does not pay for every conversation's laterals. Page with `next_cursor`: pass it back as `cursor` until the response returns null. The cursor is a keyset on `last_activity_at`, not an offset, so conversations that become active while paging do not shift the pages already read. Keep filters identical across a paging run or the pages describe different lists.
         */
        get: operations["listConversations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/conversations/{conversation_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read one conversation and its inference events
         * @description Returns the same conversation detail the admin SPA renders at `/conversations/{conversation_id}`, plus per-event `cost_state` and `billing_class`, the optional `provisioned_capacity_evidence` that explains a provisioned classification, and the rate snapshot each estimate used.
         *
         *     Each event also carries how its upstream response ended: `terminal_state` over the closed vocabulary `completed`, `incomplete`, `truncated`, `failed`; `finish_reason`, the provider's own stop reason recorded raw and unnormalized; and `tool_call_count`. All three are null when unrecorded — the event predates the vocabulary, or the path that served it reports no terminal state — and a null is never a completed response, nor is a null `tool_call_count` a counted zero.
         *
         *     `projects` is the set of repositories this conversation has been observed working in (ADR 0059) — host-stripped `owner/repo`, never a path or a prompt — and `repo_slug`, `repo_ref`, and `workspace_name` are the latest of them. Those three are display handles only: a conversation resumed in a second clone reports the newest slug while its earlier events keep the one they were stamped with, so booking this conversation's whole cost to `repo_slug` misattributes it. `project_slug_count` above one says the handle is one of several; spend by repository is a `listUsage` question.
         *
         *     `route_pin` is the conversation's newest live route-affinity pin (ADR 0056), or null when none holds. It rides this operation's access rule, so a conversation's owner reads their own pin; `listRouteAffinity` returns every pin a conversation holds and requires an administrator. The aggregate cost fields stay split between metered, subscription, provisioned-capacity, and prepaid-credit usage; the latest-turn fields use the same partition.
         *
         *     Administrators read any conversation; every other caller reads only their own, and a conversation belonging to someone else answers 404. At most 500 events are returned, newest first, with `events_truncated` set when the conversation holds more. `listConversations` finds the conversation to read.
         */
        get: operations["getConversation"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost-controls/settings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Get cost-control settings
         * @description Reads the installation's cost-window time zone: the calendar every daily, weekly, and monthly cost limit opens and closes by. Every `window_start` and `resets_at` in `listCostLimits` is derived from it, so this is what explains a window that reset at an unexpected hour. Administrator-only.
         */
        get: operations["getCostControlSettings"];
        /**
         * Update cost-control settings
         * @description Sets the time zone every cost window is measured in, installation-wide. `time_zone` must be an IANA name PostgreSQL knows, such as `America/New_York` or `UTC`; anything else is refused with 422 rather than stored.
         *
         *     This moves the boundaries of every existing window rather than starting a new one: current spend is recomputed against the windows the new zone implies, so a policy near its cap can read differently immediately afterwards. Read the effect with `listCostLimits`.
         */
        put: operations["updateCostControlSettings"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost-limits": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List cost limits
         * @description Lists every cost limit in the installation with its live state: the spend accumulated in the current window, whether the warning threshold or the cap itself has been reached, and when the window resets. Administrator-only; a non-administrator reads its own applicable caps with `getMyCostLimitStatus`.
         *
         *     Amounts are integer micro-US-dollars — 1 000 000 to the dollar. `source` separates the two kinds of row: `database` policies are the ones `createCostLimit` makes and `updateCostLimit` and `deleteCostLimit` accept, while `configuration` rows are the deployment's sandbox spend bounds, reported here so a reader sees every cap that can deny a request but rejected by those operations.
         */
        get: operations["listCostLimits"];
        put?: never;
        /**
         * Create a cost limit
         * @description Creates one calendar-window spend cap over a scope. Limits are integer micro-US-dollars — 1 000 000 to the dollar — so a $50 monthly cap is `limit_microusd: 50000000`. A limit of `0` is a total denial of the scope's metered inference, not "unlimited"; to stop enforcing a cap without removing it, create or update it with `enabled: false`.
         *
         *     `scope_type` selects what `scope_id` names: an account id from `listPeople` for `user`, a team id from `listTeams` for `team`, a catalog model id from `listProviderModels` for `gateway_model`. The `installation` scope covers the whole gateway and takes no `scope_id` — send it null or omit it. `connected_app` is reserved: the vocabulary carries it because policy rows can hold it, but a create is refused until inference spend can be attributed to an app.
         *
         *     `applies_to` splits the traffic a cap sees: `all` covers every metered request in scope, `sandbox` covers only gateway-hosted sandbox spend. One policy exists per scope, `applies_to`, and `window`; a second create over the same three is refused with 409 naming the policy that already holds them, which is the one to change with `updateCostLimit`. Neither the scope nor the window can be re-targeted afterwards — `updateCostLimit` changes only the amount and the switch — so moving a cap means `deleteCostLimit` and a fresh create.
         *
         *     Spend is evaluated against the caller-independent window that `getCostControlSettings` names a time zone for, and the new policy's current state is returned with it: read `current_spend_microusd`, `exceeded`, and `resets_at` to see whether it bites immediately.
         *
         *     Every refusal names the rule that failed, so none of them is worth a blind retry: `scope_id_required` and `scope_id_not_allowed` for a scope and target that disagree, `unknown_scope_id` for a target no row answers to, `negative_cost_limit` for an amount below zero, `connected_app_attribution_unavailable` for the reserved scope, and `duplicate_cost_limit` — the 409 — for a tuple already covered.
         */
        post: operations["createCostLimit"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost-limits/{policy_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Update a cost limit
         * @description Changes a cost limit's amount and whether it is enforced. Nothing else: a policy cannot be re-targeted, because its scope, traffic split, and window are the identity the uniqueness rule is stated over — move a cap by calling `deleteCostLimit` and `createCostLimit` instead.
         *
         *     `limit_microusd` is micro-US-dollars — 1 000 000 to the dollar — and `0` denies the scope's metered inference outright rather than meaning "unlimited"; `enabled: false` is how a cap stops biting while its configuration is kept. Both fields are required and replace what is stored, so read the policy from `listCostLimits` before sending one. Spend already accumulated in the current window is re-evaluated against the new amount before the response is built, so a cap lowered below it comes back with `exceeded` already true.
         *
         *     Only `database` policies are updatable; the deployment's `configuration` sandbox bounds answer 404 here and change with the deployment.
         */
        put: operations["updateCostLimit"];
        post?: never;
        /**
         * Delete a cost limit
         * @description Removes a cost limit, along with the window state tracked for it. The scope stops being capped by this policy immediately; any other policy covering the same traffic — a wider scope, another window — still applies, so deleting one cap is not the same as lifting the limit on a scope.
         *
         *     This is also how a cap is re-targeted, since `updateCostLimit` changes only the amount and the switch: delete, then call `createCostLimit` with the scope, traffic split, or window you meant. To stop enforcement while keeping the policy and its history, update it with `enabled: false` instead. Only `database` policies are deletable; the deployment's `configuration` sandbox bounds answer 404 here.
         */
        delete: operations["deleteCostLimit"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost-limits/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Get the caller's applicable cost limits
         * @description Returns the cost limits that can deny the calling account's own requests — its user-scoped caps, the caps on teams it belongs to, the caps on models granted to it, and the installation-wide ones — each with the spend accumulated in the current window and when that window resets. Available to any authenticated caller, which is what separates it from `listCostLimits`: an agent can ask why it is being refused, or how much room is left, without administrator rights.
         *
         *     Amounts are integer micro-US-dollars — 1 000 000 to the dollar. A policy with `exceeded` true is denying inference in this scope now, and `resets_at` says when that ends by itself. Disabled model-scoped caps are omitted, since they constrain nothing.
         */
        get: operations["getMyCostLimitStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost/findings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List advisory cost findings
         * @description Advisory only: nothing here changes configuration, and every finding is a claim about spend an administrator confirms before acting on. Which fix is right — trimming an instruction file, dropping a cache breakpoint, moving to a smaller route — is a decision the gateway cannot make.
         *
         *     Findings are drawn per (client, route) subject over the window, because fixed context belongs to whoever assembles it while price and context window belong to the route. Three rules fire:
         *
         *     `oversized_fixed_context` — the subject's median opening-request preamble (standing instructions plus tool definitions) is at least twice the installation's own median and at least 20 KB. The comparison is against this installation rather than an absolute budget; `observed` and `baseline` are those two medians in bytes, and `impact_microusd` is the share of opening-request spend the excess accounts for.
         *
         *     `cache_writes_exceed_savings` — prompt-cache write premiums beat the read discounts they bought by more than one US dollar over the window. `observed` and `baseline` are the two halves of `cache_savings_microusd`, so this cannot disagree with the figure `listUsage` reports; it says which half won.
         *
         *     `low_context_utilization` — a route with a window of 200,000 tokens or more serves requests whose median input fills 15% of it or less. `impact_microusd` is null: the saving is the price difference against a smaller route, and choosing that route is a commercial judgment.
         *
         *     Every rule has a minimum sample — 20 opening requests for the preamble comparison, 50 committed requests for the other two — so a quiet subject produces nothing rather than noise. Rows that predate request-shape capture have null counts and drop out rather than counting as zero; `shape_recorded_requests` states that denominator beside `committed_requests`.
         *
         *     Findings are ranked by accounted spend, descending. A finding the gateway cannot price sorts after every priced one, then by request volume — unpriceable is not the same as small.
         *
         *     `since` and `until` are RFC3339; `since` is inclusive, `until` is exclusive, and `until` must be later than `since`. With neither, the window is the trailing 30 days, and the bounds applied are returned.
         */
        get: operations["listCostFindings"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/cost/state-summary": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Summarize the cost-state distribution of inference events
         * @description Reports how many inference events fall into each `cost_state` (`priced`, `pending_rate`, `provisional`, `unpriceable`), overall and per model, so unpriced traffic can be found without reading the database directly.
         *
         *     Administrators count the whole installation; every other caller counts only their own events, so the same call answers "is my spend being priced" without disclosing the organization's traffic shape.
         *
         *     Filter with `model_id` to scope to one catalog model, and `since`/`until` (RFC3339) to scope to a time window over `occurred_at`. `since` is inclusive and `until` is exclusive, and `until` must be later than `since`. Only `cost_state` values actually observed in the matched window are returned.
         *
         *     With no filter this aggregates every recorded inference event in scope, which on a large installation is a full scan; pass a window when one will do.
         *
         *     `terminal_states` reports how responses ended, per model, over the same window and scoping. Unlike the `cost_state` counts these do not partition the window: `terminal_state` is nullable and is never backfilled, so events recorded before the vocabulary — or served by a path that reports no terminal state — are returned under a null `terminal_state` and must not be read as completed responses. `text_only_count` is the subset that reported a finish reason and carried a counted zero tool calls, which is the shape of a model that answered in prose where a tool call was expected.
         *
         *     `finish_reasons` reports the raw upstream terminal strings behind those states, verbatim, each beside the `terminal_state` it was classified as — the audit trail for reasons the mapping never anticipated. The reason is provider-controlled text, so rows are ordered by event count descending and capped; `finish_reasons_truncated` says when the tail was dropped, and a narrower window or model filter recovers it.
         */
        get: operations["getCostStateSummary"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/activity": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Verdict activity by policy, engine, surface, day, and optional scope
         * @description Counts recorded guardrail evaluations, grouped by policy, engine, brokered surface, and UTC day.
         *
         *     Surface is a grouping dimension, not a merged total. A policy whose flags all come from tool results is a different promotion decision from one flagging prompts, so `inference_request`, `tool_call`, `tool_result`, and `app_egress` are counted apart; pass `surface` to narrow to one. Add `group_by=team`, `group_by=model`, or `group_by=team,model` to split those rows by the team attributed at evaluation time and/or the requested catalog model. The filter-name aliases `team_id` and `model_id` are accepted. Grouped rows add a `key` object containing exactly the requested dimensions; recorded null attribution remains null. With no `group_by`, the response is unchanged and omits `key`.
         *
         *     Counts only — no scanned content ever appears here. Use `listGuardrailVerdicts` to see which individual requests a policy flagged.
         */
        get: operations["getGuardrailActivity"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/engines": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List guardrail engines */
        get: operations["listGuardrailEngines"];
        put?: never;
        /**
         * Create a guardrail engine instance
         * @description Creates one engine instance: a key, a kind naming a code-owned implementation, and a config document that kind compiles.
         *
         *     Validation is compilation. The same function that builds the runtime engine at admission runs here, so a config this operation accepts can never fail to compile later, and one it refuses never reaches a request.
         *
         *     `builtin.` is reserved for engines the gateway seeds; an administrator-created key starts with `custom.`.
         */
        post: operations["createGuardrailEngine"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/engines/{engine_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Update a guardrail engine instance
         * @description Replaces the instance's key, name, kind, config, and enabled state. The config is compiled before it is stored, exactly as on create.
         *
         *     `config_revision` moves only when the config document actually changes, so renaming an instance does not invalidate every compiled engine in every process. A seeded instance keeps its key and kind — its name, config, and enabled state stay editable.
         */
        put: operations["updateGuardrailEngine"];
        post?: never;
        /**
         * Delete a guardrail engine instance
         * @description Deletes one engine instance. A policy that still chains the instance makes the delete a conflict rather than silently emptying that policy's chain; the refusal names how many policies to detach it from first. Seeded instances cannot be deleted.
         */
        delete: operations["deleteGuardrailEngine"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/engines/preview": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Run an engine config against sample text without saving anything
         * @description Evaluates a saved instance or an inline config against text in the request and returns the findings with the previews a verdict would store.
         *
         *     Nothing persists. No verdict, no finding group, no engine row: this is the live editor's other half — paste a payload that should match and one that should not, and see both before saving.
         *
         *     In-process kinds only. An external kind would mean relaying the sample text to a vendor, which a preview must never do, so those kinds are refused by name rather than attempted.
         *
         *     Sample text is capped and the operation is rate limited per administrator. It grants no capability an administrator lacks.
         */
        post: operations["previewGuardrailEngine"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete all recorded guardrail event history
         * @description Deletes verdicts, derived finding groups, finding reviews, daily flag state, and prior guardrail audit entries. Policies, engines, attachments, and ignored-pattern configuration are preserved.
         */
        delete: operations["wipeGuardrailEvents"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/finding-ignores": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List ignored finding patterns */
        get: operations["listGuardrailFindingPatternIgnores"];
        /** Ignore a finding category or preview prefix */
        put: operations["ignoreGuardrailFindingPattern"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/finding-ignores/{ignore_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Stop ignoring a finding pattern */
        delete: operations["unignoreGuardrailFindingPattern"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/findings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List unique guardrail findings for review
         * @description Groups repeated matches by a non-reversible finding identity so an administrator reviews one issue instead of every occurrence. New verdicts use an installation-keyed fingerprint of the exact match; historical verdicts fall back to their already-redacted category, preview, and content kind. Dismissed groups remain available through the status filter and do not weaken request-time enforcement.
         */
        get: operations["listGuardrailFindingGroups"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/findings/{finding_key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /** Dismiss or reopen one unique guardrail finding */
        put: operations["reviewGuardrailFinding"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/policies": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List guardrail policies */
        get: operations["listGuardrailPolicies"];
        put?: never;
        /** Create a guardrail policy */
        post: operations["createGuardrailPolicy"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/policies/{policy_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Update a guardrail policy
         * @description Replaces the policy's name, mode, stance, enabled state, and engine list.
         *
         *     A longer engine list is checked against the scope engine-chain cap the same way an attachment is, because lengthening an attached policy's chain and attaching another policy grow a scope's chain by the same amount.
         */
        put: operations["updateGuardrailPolicy"];
        post?: never;
        /** Delete a guardrail policy */
        delete: operations["deleteGuardrailPolicy"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/policies/{policy_id}/attachments": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** Attach a guardrail policy to a scope */
        post: operations["createGuardrailAttachment"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/policies/{policy_id}/attachments/{attachment_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /** Detach a guardrail policy from a scope */
        delete: operations["deleteGuardrailAttachment"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/templates": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List guardrail policy templates
         * @description Returns the curated catalog: for each entry, the engines it composes with a rationale per engine, the parameters it declares, a suggested attachment scope, a version, and a maturity tag.
         *
         *     Suggestions only — nothing here is active, and a template cannot activate itself. Applying one is an explicit call to `applyGuardrailTemplate`, and what that creates is always a monitor-mode policy with a fail-open stance, whatever the caller asks for. Promotion to enforce stays a deliberate second step taken after reading the new policy's own verdicts.
         *
         *     Every rationale states what the check catches **and what it misses**. Read the second half before promoting anything: pattern detection finds records, not intent, and an entry tagged `experimental` is shipped for evaluation rather than as a control.
         *
         *     The catalog is owned by the gateway version, so an upgrade may revise an entry; it never touches policies already applied. A policy whose template has moved carries `template_divergence` in `listGuardrailPolicies`. Administrator-only.
         */
        get: operations["listGuardrailTemplates"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/templates/{template_id}/apply": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Apply a guardrail policy template
         * @description Creates the template's engine instances, its policy, the policy's engine chain, and the attachment — in one transaction, or nothing. Server-side because a template spans three record types with references between them, and a client-side sequence that failed midway would leave orphaned engines behind.
         *
         *     The policy is always created in monitor mode with a fail-open stance. This request has no `mode` or `failure_stance` field to set, which is the mechanism rather than an omission.
         *
         *     A template that declares required parameters refuses to apply without them, rather than creating a policy that matches nothing.
         *
         *     Names and engine keys carry an apply sequence, so applying a revised template beside its predecessor never collides: the second application of `us_pii` is named `US PII (2)` on the key `custom.us_pii.2`. Its findings still land in the first copy's groups, because a template's rules carry a stable identity, so existing ignores and review decisions carry over.
         */
        post: operations["applyGuardrailTemplate"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/guardrails/verdicts": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List recorded guardrail verdicts
         * @description Lists individual recorded verdicts, newest first — the drill-down behind `getGuardrailActivity`.
         *
         *     This is the promotion evidence: `getGuardrailActivity` says a monitor-mode policy flagged 40 evaluations yesterday, and this says which requests they were, on which surface, for whose traffic. Each row carries the `request_id` where the surface recorded one, so a flagged evaluation can be traced through `getInferenceRequest`.
         *
         *     `findings` carries the category, segment, offsets, and redacted detector preview. New flagged verdicts also include the complete surrounding normalized message, tool payload, or HTTP body with every detected span redacted and highlighted. Historical verdicts recorded before context retention carry metadata only.
         *
         *     Page with `next_cursor`: pass it back as `cursor` until the response returns null. The cursor is a keyset, not an offset, so verdicts recorded while paging do not shift the pages already read.
         */
        get: operations["listGuardrailVerdicts"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/identity-providers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List identity providers
         * @description Every configured provider and the terms it onboards on. No client secret is returned by any operation here; `secret_configured` reports only whether one is stored.
         *
         *     `source` decides what can be changed from this API. A `config` provider is owned by deployment configuration, and every write here refuses it with `config_managed` — it is changed by redeploying, and an edit that appeared to succeed would be reverted on the next restart. A `database` provider is administered here.
         *
         *     `enabled` and `status` are independent and both required for sign-in: a provider is offered only when it is enabled *and* verified. Verification is a redirect through the provider and back, so it is completed from the identity page rather than through this API.
         */
        get: operations["listIdentityProviders"];
        put?: never;
        /**
         * Configure a federated sign-in provider
         * @description Creates a `database` provider, disabled and unverified. Neither is a defect to correct: a provider that took sign-ins the moment it was configured would take them on terms nobody had confirmed the issuer agrees to. Completing verification is a redirect through the provider and back, so it is done from the identity page; only then does `setIdentityProviderEnabled` succeed, and until both hold the provider is not offered at sign-in.
         *
         *     `client_secret` is write-only. It is encrypted under this provider's own ID before storage, and no operation returns it — `secret_configured` reports only that one is stored. There is no operation to rotate it; a provider whose secret changed is replaced.
         *
         *     `issuer` must be `https` and is the trust root: it is what the gateway checks tokens against, so a second provider for an issuer already configured is refused with `issuer_taken` rather than created alongside. A reused name is `name_taken`.
         *
         *     `jit_enabled` with an empty `allowed_email_domains` lets anyone the issuer will authenticate obtain an account here. Set the domains, or leave JIT off and create accounts with `createPerson`.
         *
         *     Each rejected field is reported on its own: `invalid_name`, `invalid_issuer`, `invalid_client_id`, `invalid_groups_claim`, `invalid_client_secret`, `invalid_domains`, and `invalid_scopes` for a scope this gateway will not request. `openid`, `profile`, and `email` are always requested and are not listed in `additional_scopes`. An `issuer` is `invalid_issuer` unless it is an `https` URL of at most 2048 characters; any trailing slash is removed before that is judged, and before it is compared with the issuers already configured.
         */
        post: operations["createIdentityProvider"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/identity-providers/{provider_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read one identity provider
         * @description The same view `listIdentityProviders` returns, for one provider. Read this after a write to confirm the resulting state: the writes here answer with no body.
         */
        get: operations["getIdentityProvider"];
        put?: never;
        post?: never;
        /**
         * Delete an identity provider
         * @description Removes the provider outright. Refused with `linked_identities` while any account is linked to it: those links are what let those people sign in, and deleting the provider would leave the links pointing at an issuer that is no longer configured. `setIdentityProviderEnabled` with `false` is the reversible way to take a provider out of service, and it keeps the links intact.
         *
         *     A provider whose `source` is `config` is refused with `config_managed` and is reported that way before anything else, because no other refusal here can be cleared for it: deployment configuration recreates it on the next start whatever else changes.
         *
         *     Deleting is otherwise refused for the same two reasons as withdrawing — `sso_enforced` and `sso_only_accounts` — because it withdraws the provider as a side effect of removing it.
         */
        delete: operations["deleteIdentityProvider"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/identity-providers/{provider_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Offer an identity provider at sign-in, or withdraw it
         * @description A provider whose `source` is `config` is owned by deployment configuration and is refused with `config_managed`, whichever way it is being moved. Change it by redeploying; configuration is re-read on every start, so a change made here would be reverted.
         *
         *     Enabling requires a provider that has completed a verification round trip; one that has not is refused with `provider_unverified`. Verify it from the identity page — the flow is a redirect through the provider and back, which this API cannot carry.
         *
         *     Withdrawing is refused when it would leave sign-in with no way in, and the two refusals are undone differently. `sso_enforced` means the policy accepts only SSO and this is the last enabled verified provider: relax the policy first. `sso_only_accounts` means active accounts have no password and no other provider — no policy change reaches them, and they need another way in before this one closes. `getAuthenticationPolicy` reports both in advance: `login_mode` decides the first, `accounts.passwordless` the second.
         *
         *     Withdrawing does not sign anyone out or unlink any account. It stops the provider being offered, and the links are still there if it is offered again.
         */
        put: operations["setIdentityProviderEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/identity-providers/{provider_id}/provisioning": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set who an identity provider may onboard
         * @description `jit_enabled` decides whether someone who authenticates successfully but has no account here gets one created at that moment. With it off, an unmatched identity is refused at sign-in and the account has to be created first with `createPerson`.
         *
         *     `allowed_email_domains` narrows that further: with it non-empty, only a verified email claim in one of the listed domains may create an account. It is a list of bare domains — `example.com`, not `@example.com` or a URL — lowercased and de-duplicated on the way in, with at most 100 entries. An empty list means unrestricted, which with `jit_enabled` on lets anyone the provider will authenticate hold an account here; that is the right setting only for a provider whose own audience is already this installation's.
         *
         *     Neither setting affects accounts that already exist, or people already signed in. Both are read at the moment an unmatched identity arrives.
         *
         *     A provider whose `source` is `config` carries both settings in deployment configuration and is refused with `config_managed`; change it by redeploying.
         */
        put: operations["setIdentityProviderProvisioning"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/inference-requests/{request_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Trace one request ID to its recorded inference events
         * @description Looks up the usage events recorded under one client-visible request ID — the `x-request-id` the gateway echoes on every inference response, minted server-side when the caller sends none.
         *
         *     Each event carries the request's disposition (`outcome`, `error_code`, `upstream_status`, `duration_ms`), its usage and cost snapshot including `billing_class` and the optional `provisioned_capacity_evidence` that explains a provisioned classification, how the response ended (`terminal_state` over `completed`, `incomplete`, `truncated`, `failed`; the raw `finish_reason`; `tool_call_count` — all null when unrecorded, and a null is never a completed response), the shape of the turn behind it (`message_count`, `tool_definition_count`, `context_window`), which credential served it and why (`credential_class`, `subscription_binding_id`, `subscription_label`, `subscription_owner_email`, `fallback_from_subscription`, `credential_resolution_reason`), the `conversation_id` it was attributed to, from which `getConversation` reads the surrounding turns, and the `guardrail_verdicts` recorded at the admission gate (policy, engine, decision, action, category/position findings, and the complete surrounding matched segment with every detected span redacted).
         *
         *     `attempt_role` is what makes this a failover trace rather than a lookup: the gateway records one event per attempted route, all under the same request ID, so `primary` or `failover` names the attempt that became the answer and `abandoned` names a held response the waterfall walked past. A null is unrecorded — rows predating the vocabulary — and never a committed attempt. `getModelWaterfall` reports the configured order and each route's health, which is where the reason for a failover is read.
         *
         *     Administrators trace any request; every other caller traces only their own. An ID that was never recorded and an ID belonging to someone else both return an empty `events` list. A caller-supplied header that is not a UUID is echoed on responses but recorded as nothing, so it is rejected here by name rather than answering the empty list an unrecorded UUID gets. The ID is caller-suppliable, so it may name more than one event: all are returned newest first, capped, with `events_truncated` set when the cap dropped some.
         */
        get: operations["getInferenceRequest"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/issues": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List deduplicated operator issues
         * @description Returns grouped causes instead of raw failure rows, so the administrator console and diagnostic agents use the same classification. Groups cover platform-owned background-task failures, inference events awaiting rates, permanently unpriceable events, and advisory cost findings.
         *
         *     Every group carries a stable content-derived key, a machine-readable cause, a disposition (`actionable` or `unavoidable`), a severity, occurrence count, first and last occurrence, current route status, copyable identifiers from the latest exemplar, bounded structured evidence, a suggested next action, and whether an administrator dismissed it.
         *
         *     System failures deduplicate by task kind, lifecycle stage, and stable failure code. Their bounded detail and diagnostics remain evidence, so variable provider excerpts do not split one cause into many groups. Pending-rate groups deduplicate by route and the exact missing rate components. Missing-usage groups deduplicate by route, outcome, error code, and missing usage fields. Cost findings use their existing per-client, per-route subjects.
         *
         *     A successful response without input or output usage is actionable while its route remains active. Failed requests without usage, and history on a removed route, are unavoidable because no successful provider response existed to report token counts. Active routes with missing rates are actionable. Causes that match a disabled feature, a disabled route, a corroborated subscription cooldown, or a denied request are omitted rather than listed as expected.
         *
         *     Administrators only: the groups describe installation-wide failures and traffic shape. Dismissed groups stay hidden unless `include_dismissed=true`; the summary always counts active groups only. `since` is inclusive and `until` is exclusive over occurrence time. With no bounds the read covers all retained history, which can scan the inference ledger. The response is capped at 500 groups after actionable-first ordering; `truncated` tells the caller to narrow the window.
         */
        get: operations["listIssues"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/issues/{issue_key}/dismissal": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Dismiss one stable operator issue
         * @description Stores the issue's stable key. Present and future occurrences with the same key stay out of active issue reads until an administrator restores the issue.
         */
        put: operations["dismissIssue"];
        post?: never;
        /** Restore one dismissed operator issue */
        delete: operations["restoreIssue"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List MCP endpoints
         * @description Every endpoint with the apps it exposes and the trust tier it exposes them on. User-owned endpoints are included so that an installation-wide view is complete, and are identified by `owner_type`; the write operations here refuse them with `user_owned_endpoint`.
         *
         *     `app_ids` are connected-app IDs; resolve their names and policies with `listConnectedApps`.
         */
        get: operations["listMcpEndpoints"];
        put?: never;
        /**
         * Create an MCP endpoint
         * @description The endpoint is created disabled: assign the apps it should expose, confirm them, then enable it with `setMcpEndpointEnabled`. A client pointed at a slug that is not yet serving what was intended is harder to correct than one that cannot connect yet.
         *
         *     `slug` becomes the URL clients connect to, at `/mcp/{slug}`, and must be lowercase letters, digits, and dashes, not starting with a dash. A slug another *live* endpoint holds is refused with `slug_taken`. Deleting an endpoint releases the slug it had and expires access tokens minted for `mcp:{slug}` / `runtime:{slug}` — the soft-deleted row stays so audit history still resolves by id, but the slug is free for a new endpoint.
         *
         *     `execution_mode` is the trust tier. `direct` lets any authorized MCP client invoke the assigned apps; `gateway_attested` admits only tool calls this gateway attested. Creating a Direct endpoint that carries a model-restricted app, or the app the gateway provisions for itself, is refused with `direct_execution_conflict`: the route the call is being made on is what the restriction is about, and on Direct nothing is checking it.
         */
        post: operations["createMcpEndpoint"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read one MCP endpoint
         * @description The same view `listMcpEndpoints` returns, for one endpoint. Read this after a write to confirm what the endpoint now exposes: the write operations answer with no body, and the trust tier an endpoint ends up on can be moved by a connected-app policy change made elsewhere.
         */
        get: operations["getMcpEndpoint"];
        put?: never;
        post?: never;
        /**
         * Delete an MCP endpoint
         * @description Removes the endpoint and its app assignments. Every client configured against the slug stops being served, with no equivalent of a redirect — `setMcpEndpointEnabled` is the reversible way to close an endpoint while its clients are reconfigured.
         *
         *     The slug is released and live access tokens for `mcp:{slug}` and `runtime:{slug}` are expired. Audit and usage history still resolve through the soft-deleted row by id. A new endpoint may reuse the slug; a client that re-authorizes afterward binds to that new endpoint, while tool listing still re-derives ownership from live state.
         *
         *     An endpoint a user created for their own client is refused with `user_owned_endpoint`. Removing it would break a client only that person configured, so it is theirs to remove.
         */
        delete: operations["deleteMcpEndpoint"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}/apps/{app_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Add or remove one app on an endpoint
         * @description Addresses one app at a time rather than replacing the list, so two administrators working on the same endpoint do not silently undo each other's assignment.
         *
         *     Adding a model-restricted app, or the app the gateway provisions for itself, to an endpoint on the Direct tier is refused with `direct_execution_conflict`. Move the endpoint to `gateway_attested` with `setMcpEndpointExecutionMode` first. Removing an app is never refused on those grounds — it can only narrow what the endpoint exposes.
         *
         *     Removal takes effect immediately, including for executions this gateway already authorized against the endpoint: the assignment is checked again on the way out, so a call in flight resolves as revoked rather than completing under access that has been withdrawn. Removal is still checked against both IDs, so an `app_id` that names nothing is reported as `unknown_app` rather than accepted as done.
         *
         *     An endpoint a user created for their own client is refused with `user_owned_endpoint`. What that endpoint exposes is theirs to decide, in either direction.
         */
        put: operations["setMcpEndpointApp"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Let clients connect to an endpoint, or stop them
         * @description Disabling closes the endpoint to clients while keeping its slug, its app assignments, and its tier — the reversible way to stop it, and the one to reach for while an assignment is being reconsidered. `deleteMcpEndpoint` is the other, and it releases the slug.
         *
         *     In-flight executions the gateway already authorized against this endpoint stop being served: they resolve as revoked rather than completing under a decision that has been withdrawn.
         *
         *     An endpoint a user created for their own client is refused with `user_owned_endpoint`. Closing it would break a client only that person configured, so it is theirs to close.
         */
        put: operations["setMcpEndpointEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}/execution-mode": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Move an endpoint between trust tiers
         * @description `gateway_attested` admits only tool calls this gateway attested, so every invocation carries a model route it can be authorized against. `direct` lets any authorized MCP client invoke the assigned apps, which is what makes it unsuitable for an app whose policy is about routes.
         *
         *     Moving to `direct` is refused with `direct_execution_conflict` while the endpoint carries a model-restricted app or the app the gateway provisions for itself. Nothing is changed. Detach that app with `setMcpEndpointApp`, or widen its policy to `all` with `setConnectedAppModelPolicy`, and the move is admissible — the refusal names a state, not a permanent property of the endpoint.
         *
         *     The reverse move is always allowed: attestation only ever narrows what may invoke.
         *
         *     An endpoint a user created for their own client is refused with `user_owned_endpoint`, which is a property of the endpoint rather than a state — no change to any app makes the move admissible.
         */
        put: operations["setMcpEndpointExecutionMode"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}/name": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Rename an MCP endpoint
         * @description Changes only what administrators read. The slug is the address clients are configured against and is not renamable, so nothing a client resolves changes here — recreate the endpoint under a new slug when the address itself has to move.
         *
         *     A name that is empty after trimming, or longer than 120 characters, is refused with `invalid_endpoint_name` and nothing is changed.
         *
         *     An endpoint a user created for their own client is refused with `user_owned_endpoint`. Its name appears in that person's own view, so it is theirs to change.
         */
        put: operations["setMcpEndpointName"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/mcp-endpoints/{endpoint_id}/sandbox-profiles/{profile_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Attach or detach one sandbox profile on an endpoint
         * @description An endpoint whose administrator has attached at least one profile exposes the sandbox spawn tools; an endpoint with none does not, so attachment is the gate that decides which clients can start a sandbox at all and from which specifications.
         *
         *     Addresses one profile at a time rather than replacing the list, so two administrators working on the same endpoint do not silently undo each other's attachment. Detaching takes effect immediately: a spawn quoting a detached profile is refused by the same statement that would have recorded it, so a configuration change cannot be outrun by a request already in flight.
         *
         *     A disabled profile may be attached — nothing spawns from it until `setSandboxProfileEnabled` opens it — because attachment and enablement are separately reversible decisions and collapsing them would make "pause this profile" mean "rebuild its attachment set later from memory".
         *
         *     Self-administrable (ADR 0040): putting a pod specification in front of clients is part of profile administration, not a run mutation.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        put: operations["setMcpEndpointSandboxProfile"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/me/provider-subscriptions/{binding_id}/fallback": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set my provider-subscription fallback
         * @description Sets whether metered credentials may serve after this caller-owned subscription is unavailable. Administrator role does not permit changing another account's preference.
         */
        put: operations["setProviderSubscriptionFallback"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/me/provider-subscriptions/{binding_id}/label": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set my provider-subscription label
         * @description Trims and sets the label used to distinguish this caller-owned account. Labels contain 1 to 120 Unicode characters and are unique case-insensitively among the caller's active accounts for the provider.
         */
        put: operations["setProviderSubscriptionLabel"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/me/provider-subscriptions/{binding_id}/reset-credits": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Reset my provider-subscription credits
         * @description Consumes an owner-authorized provider reset credit. Shared bindings are not eligible even when the caller may use them for inference. `redeem_request_id` is forwarded as the provider idempotency key and the attempt is audited.
         */
        post: operations["resetProviderSubscriptionCredits"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/me/provider-subscriptions/{binding_id}/shares/{team_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Share my provider subscription with a team
         * @description Creates one team grant on a caller-owned active binding. The target may be any existing team; the caller does not need to belong to it. An administrator must have enabled subscription sharing for the provider. An optional body sets per-window owner reserves (ADR 0099). Repeating an existing grant with the same policy is idempotent and does not duplicate its audit event.
         */
        put: operations["shareProviderSubscriptionWithTeam"];
        post?: never;
        /**
         * Stop sharing my provider subscription with a team
         * @description Deletes exactly one team grant from a caller-owned binding. Withdrawal is allowed even when provider sharing has since been disabled. Other team grants, the binding itself, and every other account are untouched.
         */
        delete: operations["unshareProviderSubscriptionFromTeam"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/me/provider-subscriptions/{binding_id}/usage/refresh": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Refresh my provider-subscription usage
         * @description Runs the same owner-driven Check action as the account surface: refreshes the provider credential when possible and forces the provider-specific usage reread used for xAI billing snapshots. For Anthropic, whose usage arrives only on response headers, a successful check clears recorded cooldowns so the next request re-evaluates the limit state. The binding must belong to the caller; administrator role does not widen the lookup.
         */
        post: operations["refreshProviderSubscriptionUsage"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List model providers
         * @description Returns every configured provider.
         *
         *     `openai_compatible_api` reports which `OpenAI` API a provider is served southbound — `responses`, forwarded untouched, or `chat_completions`, translated by the gateway in both directions. It is null for every kind that has no such choice, which is every kind but `openai_compatible`: those reach a fixed upstream whose surface the gateway already knows. Change the field with `setModelProviderOpenAiApi`.
         */
        get: operations["listModelProviders"];
        put?: never;
        /**
         * Configure a model provider
         * @description Creates the provider a catalog model will hang off. Add the provider first, then its models with `createProviderModel`.
         *
         *     The required fields depend on the combination you choose. Bedrock and Vertex require `workload_identity` plus a region and a cloud identity mode; `assume_role` additionally requires `role_arn`, and `service_account_impersonation` requires `gcp_service_account_email`. Sending a field the chosen mode does not use is rejected rather than ignored, because a stored credential that is never applied reads as trust that is in force when it is not.
         *
         *     A new provider is enabled; its models still have to be created and granted before anything routes to it. The API key is encrypted before storage and no read returns it.
         */
        post: operations["createModelProvider"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete a model provider
         * @description Deletes the provider, every catalog model under it, and the team grants that reference those models. Access removed this way is not recorded anywhere it can be restored from and has to be rebuilt by hand, so disable the provider instead unless it is genuinely being retired.
         *
         *     Recorded inference events survive: those already priced keep their recorded cost, and those still awaiting a rate are marked unpriceable rather than deleted.
         */
        delete: operations["deleteModelProvider"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/api-key": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set the provider's installation API key
         * @description Stores the key this installation authenticates to the provider with, encrypted at rest. Use it to give an installation credential to a provider that was created without one — a provider standing only on its users' own consumer subscriptions — and to replace the key on a provider that already has one. A provider created with `auth_method` `none` becomes an `api_key` provider in the same call; nothing has to be deleted and recreated to gain a key.
         *
         *     Idempotent replacement, so the response is the same whether a key was already there. The key is never returned by this or any other read, and does not appear in the audit ledger — `listModelProviders` reports only whether a credential is configured. Send the key exactly once and keep your own copy if you need one.
         *
         *     Bedrock and Vertex providers present a workload identity instead and refuse with `provider_kind_unsupported`, as does the development fixture. An empty or whitespace-only key is refused with `credential_required` rather than being read as a request to leave the current key alone.
         *
         *     Removing a key is not offered. A provider left with no credential while models still route through it fails every request; disable the provider with `setModelProviderEnabled` instead.
         */
        put: operations["setModelProviderApiKey"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/bedrock-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set a Bedrock provider's project and retention policy
         * @description Chooses the Bedrock project this provider routes through and the most permissive retention mode it will accept from that project: `none` requires zero durable retention, `aws_only` permits AWS-managed retention but no sharing with the model publisher, and `provider_share` permits both. Inference is refused at request time if the project's live mode exceeds what is set here, so tightening this can stop traffic that was serving a moment ago.
         *
         *     Both fields are replaced together. Read `getModelProviderPolicy` first and send back the value you do not mean to change. A provider of any other kind is refused with `provider_kind_unsupported`, and a project the installation will not route through is refused on the `mantle_project_id` field as `invalid_mantle_project_id` when it is malformed or `bedrock_project_not_approved` when the deployment pins its projects and this is not one of them.
         *
         *     The project is read from AWS before anything is stored, so a project the installation's credential cannot see is refused here rather than on the first inference request. That makes this slower than the rest of this API and able to fail for reasons on AWS's side: a `502` carrying `provider_access_failed` is worth retrying, and the `409` and `422` are not. Every attempt is recorded in the audit ledger.
         */
        put: operations["setBedrockProviderPolicy"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/discovery": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * List the models a provider's account can serve
         * @description Asks the provider's upstream what models it can serve and reports them, along with whether each one is usable and what is missing when it is not. Nothing is added to the catalog: use `createProviderModel` or `importProviderModels` for the ones you want, which is where the gateway ID, limits, and rates are chosen.
         *
         *     Bedrock and Vertex enumerate their cloud account with the installation's workload identity. Anthropic, OpenAI, and OpenAI-compatible providers enumerate with their stored API key. Anthropic reports account-visible limits and capabilities; OpenAI reports identity. Fireworks endpoints are read from the Fireworks management catalog (context, capabilities, and lifecycle; no rates), Together endpoints from its enriched models listing (context and rates), and any other endpoint from its plain `/v1/models` (identity only). Providers with `auth_method: none` are refused with `discovery_unsupported` rather than returning an empty list, because an empty list reads as 'this account offers nothing'.
         *
         *     Fireworks discovery also reads the public inference listing so that fast-router variants (e.g. `accounts/fireworks/routers/kimi-k3-fast`) are included alongside the management catalog. The router listing carries their capabilities; rates are absent from both Fireworks listings, so they must still be supplied from the pricing page or the gateway's published snapshot.
         *
         *     This reaches a third party over the network, so it is slower than the rest of this API and can fail for reasons on their side. A `502` carrying `provider_access_failed` means the upstream refused the call or could not be reached and is worth retrying; the `409` is not. Every attempt is recorded in the audit ledger whether it succeeded or not.
         */
        post: operations["runModelProviderDiscovery"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enable or disable a provider
         * @description Disabling a provider stops every model under it from serving traffic while leaving the models and their team grants intact. Prefer it to `deleteModelProvider`, which removes both.
         */
        post: operations["setModelProviderEnabled"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/name": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Rename a provider
         * @description The name is how a provider is addressed in every listing, and the console joins a model row to its provider by it, so no two providers may share one. A name another provider already answers to, ignoring case, is refused with `duplicate_provider_name` and nothing is changed.
         *
         *     A name that is empty after trimming, or longer than 120 characters, is refused with `invalid_provider_name`.
         *
         *     This changes nothing about routing: `provider_kind`, `endpoint_url`, `auth_method`, and every model under the provider are untouched.
         */
        put: operations["setModelProviderName"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/openai-api": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set a provider's OpenAI API
         * @description Changes only the southbound API spoken by an `openai_compatible` provider. Choose `responses` when requests can be forwarded untouched, or `chat_completions` when the gateway must translate requests and responses. The next request uses the new value.
         *
         *     Other provider kinds have fixed upstream APIs and are rejected with `openai_api_unsupported`; their stored default is not a configurable setting. This operation does not change `endpoint_url`, `provider_kind`, or `auth_method`.
         */
        put: operations["setModelProviderOpenAiApi"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a provider's retention and subscription policy
         * @description Returns what this provider is currently permitted to do, which is what the three policy mutations change. Read it before setting a policy: `setBedrockProviderPolicy` replaces both of its fields at once, so sending one without the other's current value silently changes it.
         *
         *     The `bedrock_*` fields are null on providers that do not speak Bedrock. Null there means not applicable, not unset.
         */
        get: operations["getModelProviderPolicy"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/subscription-policy": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set whether users may attach their own subscription
         * @description `subscription_preferred` lets each user attach their own consumer subscription and prefers it over the installation's metered credential for that user's traffic; `disabled` means only the installation credential is ever used. Only Anthropic and `OpenAI`-shaped providers can carry subscriptions; any other kind accepts `disabled` and refuses anything else with `provider_kind_unsupported`.
         *
         *     Setting `disabled` does not revoke the subscriptions already attached — it stops them being selected. `listProviderSubscriptions` shows what is attached and `revokeProviderSubscription` destroys one.
         *
         *     `fallback_default` and `cli_only` are replaced along with the policy. `fallback_default` is the starting metered-fallback preference for subscriptions attached after this call and does not change the ones already attached; `cli_only` applies immediately to every binding and confines subscription use to requests an attributed CLI session vouched for, which is how an installation keeps a personal plan from serving automated traffic.
         *
         *     `sandbox_excluded` applies immediately and keeps a gateway-hosted sandbox's delegated session from resolving a subscription. It defaults to permissive, matching the decision that a sandbox is the user working, and is separate from `cli_only` because a sandbox session satisfies that control technically — a spawn refused by this flag fails preflight with `subscription_sandbox_excluded`, not at its first inference request.
         *
         *     `sharing_enabled` is the opposite: omitting it leaves the current value. It admits team-shared bindings — one user's subscription serving a teammate's overflow — and switching it off immediately refuses every borrowed session that is pinned to one. Off by default; sharing a consumer subscription across individuals is a terms-of-service judgment the installation makes, not the gateway.
         */
        put: operations["setModelProviderSubscriptionPolicy"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/subscriptions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List the subscriptions attached to a provider
         * @description Every binding under this provider, active and revoked, with the user it belongs to, the owner's label for it, and whether the provider currently considers it usable. This is where `revokeProviderSubscription` gets its `binding_id`.
         *
         *     Administrator-only: it names which individuals attached a personal subscription and which upstream account each one belongs to. `limit_state` is the provider's own verdict — `cooling_down` is a throttle that lifts on its own, while `exhausted_reauth` needs the owner to reauthorize and no administrator action will clear it.
         *
         *     `limit_state` alone cannot answer whether an account is still exhausted or has already reset, so the temporal state is here too: `cooldowns` says when each throttle lifts, `usage_windows` carries the provider's own quota meters as last observed, and `usage_snapshot_at_unix_seconds` says how old that observation is. Read the age first — a window whose `resets_at_unix_seconds` has passed describes quota that has since come back. Where `usage_supported` is false the provider publishes no usage the gateway can read and `usage_windows` is empty on every binding, which is a different thing from an account that has served nothing.
         *
         *     `borrowed_request_count` is counted over `since`/`until`, defaulting to the trailing 30 days when neither is given. Send the same bounds the console is showing when comparing the two.
         *
         *     No credential material is returned by this or any other read, and neither are the raw provider response headers behind the usage windows.
         */
        get: operations["listProviderSubscriptions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/subscriptions/{binding_id}/reset-limit-state": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Let a cooling-down subscription be re-evaluated
         * @description Forgets the cooldowns recorded against one binding and returns it to `available`, so the next request routed to it re-evaluates the account against the provider instead of waiting out a deadline.
         *
         *     This does not grant quota and does not contradict the provider: a cooldown that is still true is re-earned on the next request, scoped by that response's own headers. It exists because a cooled binding is never sent the request that would prove it recovered, so a cooldown recorded from thin evidence — an unattributable 429, or a reset deadline the provider has since moved — holds paid capacity offline with nothing able to falsify it. A plan change is the common cause: the stored windows describe the plan the account used to have.
         *
         *     The owner can do this for their own binding with `refreshProviderSubscriptionUsage`, which is the same forgetting reached through a credential check. Use this when the owner is unavailable, which is the usual case for a shared account that has parked itself.
         *
         *     Answers `204` whether or not anything was recorded, because the question it settles — may this account take traffic again — is answered yes either way. The audit ledger records `provider_subscription.limit_state_reset` only when state actually changed.
         */
        post: operations["resetProviderSubscriptionLimitState"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/subscriptions/{binding_id}/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Revoke one user's attached subscription
         * @description Marks the binding revoked and destroys the stored access and refresh tokens. The user's traffic falls back to the installation's metered credential where the provider allows it and is refused where it does not, so this can stop that one user serving traffic.
         *
         *     Not reversible from here and not a pause: the user has to reauthorize with the provider to attach a new subscription, which no administrator can do on their behalf. To stop subscriptions across the whole provider without destroying anyone's credential, set `setModelProviderSubscriptionPolicy` to `disabled` instead.
         *
         *     The binding row survives as a revoked entry in `listProviderSubscriptions`, which is why a second call answers `409` with `subscription_already_revoked` rather than reporting a revocation that did not happen.
         */
        post: operations["revokeProviderSubscription"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-providers/{provider_id}/zero-data-retention": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enforce zero data retention on a Bedrock project
         * @description Writes zero durable retention onto the provider's Bedrock project in AWS. This is the other half of `setBedrockProviderPolicy`: that one records what the gateway will accept, this one makes the account match. A provider whose policy is `none` while its project retains data serves nothing, because every request is refused at routing time — this is how that is repaired without leaving the API.
         *
         *     Refused with `retention_policy_not_zero` unless the provider's recorded policy is already `none`, so that the account is never tightened past what the installation has agreed to. Set the policy first, then call this. A provider that does not speak Bedrock has no such project and is refused with `provider_kind_unsupported`.
         *
         *     Reaches AWS, so a `502` carrying `provider_access_failed` means the change was not made and is worth retrying. Every attempt is recorded in the audit ledger.
         */
        post: operations["enforceBedrockZeroDataRetention"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/model-tasks/conversation-title": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read the conversation-title model route
         * @description Returns the logical gateway model used by the asynchronous conversation-title task. The selected model's existing failover waterfall supplies fallback routes. Model calls run under each conversation owner's grants, limits, and ordinary credential resolution.
         */
        get: operations["getConversationTitleModelTask"];
        /**
         * Set the conversation-title model route
         * @description Selects one enabled logical gateway model for asynchronous conversation titles. The task uses that model's failover waterfall and runs under each conversation owner's model grants, cost/rate limits, and credential resolution. An eligible user subscription is preferred; otherwise the installation provider credential serves the request and usage remains attributed to the user.
         */
        put: operations["setConversationTitleModelTask"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List catalog models
         * @description Read the catalog before adding a model: `gateway_id` is globally unique, and an existing row should be repaired in place rather than deleted and recreated.
         *
         *     Filters combine with AND and may be omitted. `provider_id` scopes to one provider, `enabled` to models that may or may not serve traffic, and `pricing` to how completely rates are configured: `unpriced` means the input or output rate is null so traffic cannot be priced at all, `partial` means both base rates are set but at least one cache rate is null, and `complete` means all five are set. Every model is in exactly one `pricing` bucket.
         *
         *     `partial` is not by itself a defect: a null cache-read rate bills cached input at the input rate, and null cache-write rates leave those writes unpriced, which is correct for providers that do not charge for them. `unpriced` is the one to act on.
         *
         *     A filter that matches nothing returns an empty list, including a `provider_id` that names no provider — these narrow a listing rather than look up a resource.
         */
        get: operations["listProviderModels"];
        put?: never;
        /**
         * Add a model to the catalog
         * @description Adds one model under an existing provider.
         *
         *     Leave `context_window` and `max_output_tokens` null unless the publisher states them; they are served to clients that size token budgets from them, so an estimate causes truncated requests. Bedrock and Vertex providers require both.
         *
         *     Rejections name the rule that failed in `fields[].code`, so correct that field and retry rather than varying other values.
         */
        post: operations["createProviderModel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete a catalog model
         * @description Deletes the model and the team and person grants that reference it. Nothing in the installation records that those grants existed, so the access removed this way has to be rebuilt by hand from whatever record the operator kept outside the gateway.
         *
         *     Prefer `setProviderModelEnabled` with `enabled: false`, which stops the model serving traffic while leaving its grants in place, and `setProviderModelLimits` or `setProviderModelPricing` for repairing a model that was configured wrong. Deleting and recreating is not a repair: the new model is a different row and inherits none of the grants.
         *
         *     Recorded inference events survive. Those already priced keep their recorded cost; those still awaiting a rate become unpriceable.
         */
        delete: operations["deleteProviderModel"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/anthropic-message-batches": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set Anthropic Message Batch support
         * @description Changes the explicit Message Batch capability on one route. Enable it only for a direct Anthropic provider whose model also serves synchronous Anthropic Messages. Discovery initializes the value from Anthropic's model capabilities; use this operation to override an explicitly compatible custom endpoint or to disable a reported capability.
         */
        put: operations["setProviderModelAnthropicMessageBatches"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/canonical-id": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Set a model's canonical model ID
         * @description Records which underlying model a catalog row serves, independent of who serves it. Routing identity only: the `gateway_id` clients request, its aliases, grants, and every client-visible name are untouched.
         *
         *     This is what lets two routes back each other up — a waterfall refuses any route whose canonical model ID differs from the primary's. Setting it wrongly is therefore consequential in one direction: two rows made to agree that do not in fact serve the same model can be configured to answer for each other. Use `listWaterfallSuggestions` to see which rows look like they belong together.
         *
         *     The value must match `^[a-z0-9][a-z0-9._:-]{0,126}$`; anything else is refused with `invalid_canonical_model_id`.
         */
        post: operations["setModelCanonicalId"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/cost-multipliers": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List a model's cost multipliers
         * @description Returns the residency uplifts, Message Batch discount, and negotiated discount in force for a model, with where each value came from.
         *
         *     A residency multiplier keyed `*` applies to any pinned geography, which is how Anthropic publishes its uplift; a multiplier keyed to one geography wins over it. A request that pins no geography is not multiplied at all, and a request that pins one with no multiplier here is held unpriced rather than charged at list — those are different states and the estimate keeps them apart.
         */
        get: operations["listProviderModelCostMultipliers"];
        /**
         * Set a model's cost multiplier
         * @description Records the factor applied on top of whichever tier rate a request resolves to. Use it to enter a negotiated enterprise discount, or to override the residency uplift or Message Batch discount that the gateway seeded from what the provider publishes.
         *
         *     The change is strictly forward when a multiplier is already in force: the interval covering now is closed and a new one opens, and no already recorded event is repriced, because those events snapshot the multiplier that was true when they ran. One case reaches backward: the first residency multiplier able to cover a pinned geography at all, or the first Message Batch discount. Those writes repair requests already held unpriced for want of one. A negotiated discount and a geography a wildcard already covers apply from now.
         *
         *     `multiplier` must be greater than zero: zero erases a request's cost and a negative one inverts it, and neither is a factor anybody negotiated. Ending a discount is expressed as `"1"`, which is an entered value meaning list price, and is deliberately different from never having entered one.
         *
         *     Repeating an identical request is retry-safe: once the value and its provenance already say this, the repeat reports `unchanged` and does not carve the timeline in two.
         */
        put: operations["setProviderModelCostMultiplier"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/display-name": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Change a model display name
         * @description Changes presentation only: the gateway ID, upstream route, grants, and price history stay intact. When multiple Claude routes can appear together, include serving context such as `· Bedrock US CRIS` so Claude Code users can distinguish them.
         */
        post: operations["setProviderModelDisplayName"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enable or disable a model
         * @description Prefer disabling a model over deleting it: deletion cascades the team grants that reference it, silently revoking access that must then be rebuilt by hand.
         */
        post: operations["setProviderModelEnabled"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List the teams and accounts that hold a model
         * @description Names the teams whose grant reaches one catalog model, and every account those grants reach, where `listProviderModels` reports only how many there are.
         *
         *     An account appears once, with `granted_via` naming every granted team it belongs to and `direct_grant` saying whether it also holds the model on its own. Someone reached both ways carries both, so revoking one grant does not necessarily end their access.
         *
         *     A team is listed whenever it holds the grant, `enabled` or not. A disabled team keeps its grant row and stops conferring it, so a listed team is not by itself proof that its members can route here.
         *
         *     `granted_by` is the administrator who wrote that grant, and is null once that account is deleted or when the grant predates the record. On an account it reports the direct grant's author only; who granted a team is on the team's own row.
         */
        get: operations["listProviderModelGrants"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/limits": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Repair a model's token limits
         * @description Sets both limits on an existing model. Use this to fill in limits that were left null rather than deleting and recreating the model, which would cascade its team grants.
         */
        post: operations["setProviderModelLimits"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/prices": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List a model's rate intervals
         * @description Returns every rate interval recorded for one model, newest first, including a change scheduled but not yet in force.
         *
         *     Read this before setting rates: it shows which rates are currently applied (`effective_now`), whether a change is already pending (`scheduled`, at most one at a time), and which rates were never configured — a null rate on the effective interval is the case a dateless `setProviderModelPricing` heals backward through history.
         *
         *     A null `valid_from` marks rates that predate versioned pricing and are treated as having always applied.
         */
        get: operations["listProviderModelPrices"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/prices/{interval_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Withdraw a scheduled rate change
         * @description Removes a rate interval that has not started yet and reopens the one it was going to end. The rates in force are unaffected either way.
         *
         *     Only a pending interval can be withdrawn. An interval that has already started is history — the events it covers were priced from it — so it answers 409 rather than being deleted. That boundary can pass between reading the timeline and calling this, so treat 409 as a race, not a malformed request.
         */
        delete: operations["cancelScheduledProviderModelPrice"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/pricing": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Set a model's rates
         * @description Replaces a model's rate set in place. Use this to correct or complete rates rather than deleting and recreating the model, which cascades its team grants and detaches the recorded events the new rates were meant to price.
         *
         *     This replaces every rate rather than patching one: read the current values from `listProviderModelPrices` first and send back the ones that should not change. Input and output rates are all-or-nothing, and any cache rate requires an input rate.
         *
         *     Whether `effective_from` is present decides whether history is touched. Absent, rates that were never configured are treated as having always applied: they are written backward over the intervals missing them and those events are requeued for reconciliation. Present, the change is strictly forward and nothing already recorded is disturbed. Use a date only for a rate change announced ahead of time; leave it out to repair rates that should have been recorded all along.
         *
         *     Repeating an identical request is retry-safe: once those exact rates already describe the requested effective state, the repeat does not create another interval or a second price change.
         *
         *     Only one change may be pending at a time — withdraw the pending one with `cancelScheduledProviderModelPrice` before scheduling another.
         */
        post: operations["setProviderModelPricing"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/provisioned-capacity": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set a model route's provisioned-capacity mode
         * @description Changes only this route's capacity binding. Use `none` to preserve ordinary provider behavior. `provider_observed` is valid only for OpenAI. `route_resource` is valid only for a Bedrock `provisioned-model/*` ARN. `dedicated_only` is valid only for Vertex, except Anthropic routes on the global endpoint. `metered_only` is valid for OpenAI, Bedrock, and Vertex, but a Bedrock provisioned-model ARN must use `route_resource`.
         *
         *     Invalid combinations return one of `provisioned_capacity_resource_required`, `provisioned_capacity_global_partner_unsupported`, `provisioned_capacity_resource_conflict`, or `provisioned_capacity_mode_contract` in `fields[].code`.
         */
        put: operations["setProviderModelProvisionedCapacity"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/reasoning-efforts": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Change a model's supported reasoning efforts
         * @description Sets the per-model effort ladder harnesses clamp to: `null` means unstated (the upstream decides each request), an empty array means the model takes no effort control, and a list names every token the upstream accepts. Persisted ascending by the neutral ladder rank.
         */
        post: operations["setProviderModelReasoningEfforts"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/{model_id}/waterfall": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a model's failover waterfall
         * @description Returns the ordered fallback routes behind one gateway model, plus whether failover is switched on for it and what triggers it.
         *
         *     The gateway model itself is tier zero and is reported as `primary` rather than as a member of `routes`; `position` counts fallbacks from one. A model with no waterfall returns an empty `routes` list and the policy defaults, not a 404 — the absence of a waterfall is a state, not a missing resource.
         *
         *     `enabled` on a route is the configured bit and nothing more. Selection also requires the route's catalog row and its provider to be enabled, so a tier left switched on whose provider was later disabled is configuration that reads live and is not. `availability` reports what actually holds each tier out — `available`, `route_disabled`, `model_disabled`, or `provider_disabled` — naming the outermost cause, since re-enabling a tier under a disabled provider changes nothing.
         *
         *     `health` reports what the gateway has observed of a route recently and is null when there is nothing to report. It is carried on `primary` as well as on every fallback: a tier-zero circuit that is open is the fact that explains why traffic left the primary at all, and reporting health only on the routes it fell to names the destination without the cause. It is advisory: only a route whose circuit is open is skipped, and a waterfall whose routes are all open still serves from the primary rather than failing.
         *
         *     Which attempt actually served one request is recorded per event and read with `getInferenceRequest`, whose `attempt_role` tells the committed attempt from the abandoned ones.
         */
        get: operations["getModelWaterfall"];
        /**
         * Replace a model's failover waterfall
         * @description Writes the whole waterfall at once: the ordered routes and the policy that governs them. Array order is the priority — the first entry is tier one, tried when the gateway model's own route fails before any byte has reached the client. An empty `routes` list clears the waterfall.
         *
         *     Order and policy are one write on purpose. Saving them separately would let a failure leave a waterfall ordered as the administrator asked but failing over on a trigger they did not choose.
         *
         *     Every route must serve the same underlying model as the primary, which is what keeps failover a resilience feature rather than silent model substitution. A route that genuinely serves this model but carries a different canonical ID is repaired with `setModelCanonicalId`; find those with `listWaterfallSuggestions`.
         *
         *     Every route must also share a protocol with the primary. Selection filters the waterfall by the protocol each request arrives on, so a route with no protocol in common is configuration that can never be attempted.
         *
         *     `policy` chooses the regime (ADR 0113): `managed` follows the installation's automatic setting and must submit an empty `routes` list, since a managed model derives its routes; `custom` stores this list. Omitted keeps the stored regime, except that a non-empty list implies `custom`.
         *
         *     Refusals name the rule in `fields[].code`: `route_canonical_mismatch`, `no_shared_protocol`, `route_is_primary`, `duplicate_route`, `unknown_route`, `too_many_routes`, or `routes_under_managed_policy`.
         */
        put: operations["replaceModelWaterfall"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/failover-routing": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read the installation's automatic failover setting
         * @description Returns whether managed models derive a failover waterfall automatically (ADR 0113) and which trigger those derived waterfalls fail over on.
         *
         *     With `automatic` on, every model whose `policy` is `managed` fails over to the enabled routes sharing its canonical model ID — direct providers first, then hyperscalers, then aggregators — without anyone confirming a list. A model whose `policy` is `custom` keeps its own order and triggers whatever this setting says; `replaceModelWaterfall` moves a model between the two.
         */
        get: operations["getFailoverRouting"];
        /**
         * Set the installation's automatic failover setting
         * @description Turns automatic failover routing on or off for every managed model, and chooses the trigger derived waterfalls fail over on. Derived waterfalls are computed at selection time, so the change applies from the next request and a route added later joins them without another write. Models whose `policy` is `custom` are unaffected.
         *
         *     Including rate limits moves spend onto the fallback provider during throttling, which is why outages only is the default.
         */
        put: operations["updateFailoverRouting"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/import": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Add several models in one sweep
         * @description Creates every model in the batch under one provider, enables each one (unless `enabled` is false), and grants the listed teams and people access — one request instead of a create, an enable, and a grant per model per team.
         *
         *     Items are independent: each is validated with the same rules and stable codes as `createProviderModel`, and a rejected item skips creation, enablement, and grants for that item only. The response reports every item in submission order, so a caller retries just the rejected ones.
         *
         *     Grants are validated before anything is created: an unknown team or person rejects the whole request, because a batch that silently granted to fewer parties than asked would report success while leaving models unreachable for the missing audience.
         */
        post: operations["importProviderModels"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/route-affinity": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List live route-affinity pins
         * @description Returns the conversations and derived keys currently pinned to a route (ADR 0056), most recently served first. Strictly read-only: there is no mutation surface, and the only way to move a pin is to serve a request or let it expire.
         *
         *     `key_kind` says how the pin is held: `conversation` pins come from identity asserted in attribution headers and carry the conversation ID for linking; `derived` pins come from the ephemeral keyed digest of a headerless request's stable head and carry only a short digest prefix, because there is nothing durable to link to.
         *
         *     `expires_at` already reflects the observed cache lifetime: the configured affinity TTL is a ceiling, and a pin whose responses revealed a shorter provider cache lifetime lapses with the cache. `credential_scoped` reports whether that evidence is tied to a recorded serving credential; the credential is never exposed.
         *
         *     `conversation_id` answers "is this conversation pinned, and to what" directly. It narrows inside the query rather than filtering a page afterwards, so a conversation whose pin sits past `limit` reads as pinned rather than as absent; it returns one row per gateway model the conversation has used, and never a `derived` pin, which belongs to no conversation. A conversation's own owner reads the same pin without administrator rights on `getConversation`, which reports its newest one as `route_pin`.
         *
         *     Administrators only: pins span every user's traffic and derived rows carry no per-user identity to narrow by.
         */
        get: operations["listRouteAffinity"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/models/waterfall-suggestions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List suggested failover waterfalls
         * @description Advisory only: nothing here is configured until it is written with `replaceModelWaterfall`. Which of several equivalent routes should be preferred is a commercial decision the gateway cannot see.
         *
         *     `groups` holds sets of enabled routes that already share a canonical model ID, ordered `direct`, then `hyperscaler`, then `aggregator` — a direct surface receives features and cache behaviour first — so `members[0]` is the proposed tier zero. Each member carries its *current* canonical model ID, because reviewing a proposal means seeing what it is built on.
         *
         *     A group is still returned once one of its members has a waterfall: a route added to a group that is already partly configured is exactly the route an administrator has to be told about. `members[].has_waterfall` says which member anchors it, so a caller can offer review of the stored order rather than proposing a new one over the top of it.
         *
         *     `canonical_mismatches` holds routes that look like one model but do not group, because only Bedrock's geography infix was normalized when canonical identity was introduced: a route named for its source still claims its own gateway ID. Repair one with `setModelCanonicalId` and it joins the group on the next read. Without that repair this operation proposes nothing on an installation that is not purely Bedrock.
         */
        get: operations["listWaterfallSuggestions"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/people": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List directory users
         * @description Resolve an email address to the `user_id` that membership and grant operations take. Administrator-only: this is the installation's user directory, and the operations that consume these IDs are administrator-only too. `search` matches display name, email, or username. Use a bounded `limit` for interactive pickers.
         */
        get: operations["listPeople"];
        put?: never;
        /**
         * Create a directory account
         * @description Creates an account this gateway owns. `sign_in_mode` decides which credentials are meaningful and neither is optional in practice: `password` requires `username` and `password`, and `sso` requires both be absent — sending them with `sso` is refused rather than ignored, because an administrator who set a password expects it to work. A federated account cannot sign in until its provider asserts the same email.
         *
         *     Refused with `sign_in_mode_unavailable` when the installation's authentication policy does not admit the requested mode, or when `sso` is asked for with no identity provider enabled — that is a change to make in the authentication policy, not in this request. An email or username already in use is a `409`.
         *
         *     Creating an administrator grants full control of this installation, including over the caller. It is recorded in the audit ledger.
         *
         *     Accounts a directory provisions are not created here; they appear on first sign-in or SCIM sync.
         */
        post: operations["createPerson"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/people/{user_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Update an account's role, state, or identity
         * @description `role` and `is_active` are always applied. The three identity fields are applied only if `email` is sent, and then all three are replaced together — send back the values you do not mean to change, which `listPeople` supplies. Omit `email` to change only the role and state.
         *
         *     Deactivating an account stops it authenticating and stops its tokens working; it does not delete anything, and its recorded inference events keep their attribution.
         *
         *     Two refusals protect the installation rather than the request. `last_administrator` means the change would leave no active administrator, so nobody could undo it. `owner_protected` means the target is the installation owner, whose role and state are fixed at deployment; both are `409` and the denial is recorded.
         *
         *     Identity for an account a directory owns is refused with `directory_managed_identity`: SCIM and just-in-time provisioning rewrite the name and email on every sync, so a change accepted here would disappear at the next one. Its `role` and `is_active` are still yours to set.
         */
        put: operations["updatePerson"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/people/{user_id}/models/{model_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Grant or revoke a model for one person
         * @description A grant made directly to a person, independent of any team. It survives every membership change, which is what makes it the right tool for an exception and the wrong one for access a role should carry — `setTeamModelGrant` is where access that follows a role belongs, because it is visible on the team and lifts when someone leaves it.
         *
         *     Revoking here does not remove access the person also holds through a team.
         */
        put: operations["setPersonModelGrant"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/people/{user_id}/password": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Set an account's local password
         * @description Replaces the account's local password with the one supplied and records the reset in the audit ledger. The account is not asked to choose a new one at next sign-in, so whoever holds this value can sign in as that person until they change it — send it over a channel you would send the account itself over.
         *
         *     `owner_protected` means the target is the installation owner, whose credentials are not settable from here; the refused attempt is recorded, which is how repeated attempts become visible.
         *
         *     Use `removePersonPassword` to take a local password away rather than setting an unusable one, which would leave a credential that still authenticates if it is ever guessed.
         */
        put: operations["setPersonPassword"];
        post?: never;
        /**
         * Remove an account's local password
         * @description Takes the local credential away, leaving the account to sign in through its identity provider. The account itself survives — this is not a deletion and not a deactivation, both of which are `updatePerson`'s `is_active`.
         *
         *     Refused with `sign_in_required` when the account has no federated identity to fall back on, because removing the password would leave it unable to sign in at all rather than signing in differently.
         *
         *     `owner_protected` means the target is the installation owner; the refused attempt is recorded.
         */
        delete: operations["removePersonPassword"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/rate-limits": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List rate limits
         * @description Lists every rate policy in the installation with its live reading. For per-minute dimensions that is `utilization`, the fraction of the window currently consumed; for `concurrency` it is `in_flight`, the requests holding a slot on the node answering this call — a multi-node deployment reports one node's view, not the cluster's (ADR 0054). Administrator-only.
         *
         *     Every applicable policy is evaluated per request, so scopes do not override one another: a request meets the strictest limit that applies to it. `listRateLimitTemplates` suggests values for an installation with none of these yet.
         */
        get: operations["listRateLimits"];
        put?: never;
        /**
         * Create a rate limit
         * @description Creates one rate policy over a scope and one limited quantity. `dimension` picks the quantity and its unit: `concurrency` is requests in flight at once, `requests_per_minute` and `tokens_per_minute` are per-minute rates smoothed over the window rather than reset on the minute. Concurrency is the recommended first control for agent traffic — it stops a stuck harness on its next request instead of a minute later. `listRateLimitTemplates` carries suggested starting values with the reasoning behind them.
         *
         *     `scope_type` selects what `scope_id` names: an account id from `listPeople` for `user`, a team id from `listTeams` for `team`, a catalog model id from `listProviderModels` for `gateway_model`. The `installation` scope covers the whole gateway and takes no `scope_id` — send it null or omit it. `connected_app` is reserved: the vocabulary carries it because policy rows can hold it, but a create is refused until inference can be attributed to an app.
         *
         *     `limit_value` of `0` denies every request in scope on that dimension rather than meaning "unlimited"; to keep a policy without enforcing it, use `enabled: false`. One policy exists per scope and dimension; a second create over the same pair is refused with 409 naming the policy that already holds it, which is the one to change with `updateRateLimit`. The scope and dimension cannot be re-targeted afterwards — `updateRateLimit` changes only the value and the switch — so moving a policy means `deleteRateLimit` and a fresh create.
         *
         *     Every applicable policy is evaluated, so a narrow policy does not override a wider one: the strictest applicable limit is the one a request meets.
         *
         *     Every refusal names the rule that failed, so none of them is worth a blind retry: `scope_id_required` and `scope_id_not_allowed` for a scope and target that disagree, `unknown_scope_id` for a target no row answers to, `negative_rate_limit` for a value below zero, `connected_app_attribution_unavailable` for the reserved scope, and `duplicate_rate_limit` — the 409 — for a pair already covered.
         */
        post: operations["createRateLimit"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/rate-limits/{policy_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Update a rate limit
         * @description Changes a rate policy's value and whether it is enforced. Nothing else: a policy cannot be re-targeted, because its scope and dimension are the identity the uniqueness rule is stated over — move a policy by calling `deleteRateLimit` and `createRateLimit` instead.
         *
         *     `limit_value` is in the unit the policy's dimension names, and `0` denies every request in scope on that dimension rather than meaning "unlimited"; `enabled: false` is how a policy stops enforcing while its configuration is kept. Both fields are required and replace what is stored, so read the policy from `listRateLimits` before sending one. An edit resets the policy's window state — the stored arrival time is debt priced at the old rate — so the new value takes effect at once and without carrying the old window's overshoot across it. A lowered concurrency limit still applies from the next request, since requests already holding a slot are not cancelled.
         */
        put: operations["updateRateLimit"];
        post?: never;
        /**
         * Delete a rate limit
         * @description Removes a rate policy along with its window state. The scope stops being limited by this policy immediately; any other policy covering the same traffic — a wider scope, another dimension — still applies, so deleting one policy is not the same as lifting limiting on a scope.
         *
         *     This is also how a policy is re-targeted, since `updateRateLimit` changes only the value and the switch: delete, then call `createRateLimit` with the scope or dimension you meant. To stop enforcement while keeping the policy and its history, update it with `enabled: false` instead.
         */
        delete: operations["deleteRateLimit"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/rate-limits/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Get the caller's applicable rate limits
         * @description The self-service sibling of `getMyCostLimitStatus`, and the answer to the
         *     question a 429 raises: which policy is exhausted, how full is its window,
         *     and is it exhausted right now. Reachability, not administration — a
         *     non-administrator sees the installation policies plus the ones bound to
         *     their own account, teams, and granted models, and nothing else.
         */
        get: operations["getMyRateLimitStatus"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/rate-limits/templates": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List suggested rate-limit templates
         * @description Returns curated starting points for an installation with no rate policies yet: each template names a few scope/dimension pairs, a suggested value, and the reasoning behind that value. Suggestions only — nothing here is active, and a template cannot activate itself. Apply one by calling `createRateLimit` once per policy, adjusting values first if the installation's observed peaks say otherwise (ADR 0054 recommends a monitor-only week with limits above observed peaks before enforcing any of them).
         *
         *     Templates scoped to `user` are applied per account, so each policy needs the `scope_id` of the person it bounds. The catalog is owned by the gateway version, so an upgrade may revise the suggestions; it never touches policies already created. Administrator-only.
         */
        get: operations["listRateLimitTemplates"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-concurrency": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Get the sandbox concurrency allowance and its usage
         * @description Returns how many sandboxes may run at once — per user and across the                    whole installation — and how many are running now, so a caller can                    size a fan-out before it spawns rather than discovering the ceiling                    through a `429`. `user_running` counts the calling account's own live                    sandboxes; `installation_running` counts everyone's.
         *
         *                        Available to any authenticated caller, which is what separates it from                    the profile operations: an agent refused with                    `sandbox_concurrency_exceeded` can ask which cap bit, and how much                    room is left, without administrator rights.
         *
         *                        The counts are a point-in-time read and never a reservation. A spawn                    still counts inside the statement that admits it, so a slot this                    operation reports free can be taken by another spawn first.
         *
         *                        Occupancy is not the same as work. A sandbox the cluster has not                    placed holds its slot exactly as a running one does, so it counts in                    `user_running` and `installation_running` —                    `user_awaiting_scheduling` and `installation_awaiting_scheduling` are                    the subsets of those totals that no node has accepted yet, and                    `scheduling_pressure` carries the scheduler's own reason and message                    for the most recently updated one. An orchestrator reading a high                    occupancy with a matching awaiting count is looking at a cluster that                    cannot place pods, not at a busy installation: those slots free when                    capacity appears, not when a task finishes.
         *
         *                        Both limits are deployment configuration — `sandboxes.bounds.concurrent_per_user` and `sandboxes.bounds.concurrent_per_installation`                    — and no administrator call can widen them. Changing one means editing                    the deployment configuration and redeploying.
         *
         *                        An installation that does not run sandboxes answers `409` with                    `sandboxes_not_enabled`.
         */
        get: operations["getSandboxConcurrency"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-egress-bundles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List egress bundles
         * @description An egress bundle is an operator-owned named set of tunnel entries, each a domain with its ADR 0072 mode. A profile references bundles as required (always in its effective set) or optional (a spawn may add one by naming it). This is the domain allowlist a profile author can attach but not author.
         *
         *     `profile_names` lists the live profiles currently referencing each bundle. A bundle a profile still references cannot be deleted.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled` rather than an empty list.
         */
        get: operations["listSandboxEgressBundles"];
        put?: never;
        /**
         * Author an egress bundle
         * @description A bundle is a named set of exact-match tunnel entries, each with its ADR 0072 mode. Profiles reference it by name; the domains are authored here and nowhere else, so an administrator who can attach a bundle is narrower than one who can write any 32 hostnames.
         *
         *     The domain rules are the profile's own: exact hostnames, no duplicates, and the installation's tunnel-domain count bound.
         *
         *     A name another bundle already holds is refused with `egress_bundle_name_taken`.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        post: operations["createSandboxEgressBundle"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-egress-bundles/{bundle_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Replace an egress bundle's domain set
         * @description Replaces the whole domain set, and carries no name: profiles and spawns select a bundle by name, so a rename would silently repoint every reference. The set is replaced rather than merged, like every other security-relevant list — a merge would leave the intent to *remove* a domain expressible only as a delete-and-recreate.
         *
         *     Sandboxes already running keep the set they spawned with: the reconciler folds the bundles the row names into the pod's tunnel set once, at provision, and reincarnation re-renders from the row.
         *
         *     The domain rules are the profile's own (`invalid_tunnel_domain`, `duplicate_tunnel_domain`, `too_many_tunnel_domains`). Self-administrable like profile writes (ADR 0040). An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        put: operations["updateSandboxEgressBundle"];
        post?: never;
        /**
         * Delete an egress bundle
         * @description A bundle a live profile still references is refused with `egress_bundle_in_use`; detach it from every profile first, with `updateSandboxProfile`. References held by retired profiles are cleared by the delete itself.
         *
         *     Sandboxes already running keep the domains they spawned with.
         *
         *     Self-administrable like every other profile-adjacent write (ADR 0040). An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        delete: operations["deleteSandboxEgressBundle"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-profiles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List sandbox profiles
         * @description A profile is a complete bundle rather than a size axis: the image, the runtime class, resource requests and limits, the idle and wall-clock ceilings, the spend ceiling, the permitted harness set, and any tunnel domains travel together under one name, so a bigger sandbox cannot silently mean a weaker one.
         *
         *     Pass `endpoint_id` to list only the profiles attached to one endpoint — the set its clients may select from. Attach and detach with `setMcpEndpointSandboxProfile`.
         *
         *     Reading a profile is not authoring one: this operation is available through the built-in gateway app, and the five that write a profile are not.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled` rather than an empty list.
         */
        get: operations["listSandboxProfiles"];
        put?: never;
        /**
         * Author a sandbox profile
         * @description Authoring a profile is authoring a pod specification: it decides the image untrusted, model-authored code runs from, the isolation it runs under, and the public domains it may reach. This operation is self-administrable (ADR 0040): a model administering this installation may author profiles so it can also verify what they produce. Enablement of sandboxes themselves stays operator-only.
         *
         *     The profile is created **closed**: nothing can be spawned from it until `setSandboxProfileEnabled` opens it, and attaching it to an endpoint with `setMcpEndpointSandboxProfile` is a separate decision again. Three gates, no party holding two.
         *
         *     `image` must be digest-pinned — a repository followed by `@sha256:` and 64 hexadecimal characters. A mutable tag is refused with `image_not_digest_pinned` rather than resolved on the caller's behalf: what is stored is what is provisioned, and a tag can move between the two.
         *
         *     `runtime_class` is required and must name a hardened runtime (gVisor or equivalent). A profile without one is refused with `runtime_class_required` rather than falling back to the default runtime.
         *
         *     `tunnel_domains` are exact hostnames the proxy relays for the sandbox. Each entry is a bare string or a `{domain, mode}` object: `blind` (the default) relays the connection opaquely and records domain plus byte counts, while `intercepted` has the sidecar terminate TLS with the sandbox CA, record method + host + path + status + byte counts per request — never bodies — and re-originate upstream over verified TLS (ADR 0072). No credential injection either way. Wildcards, ports, paths, and IP literals are refused with `invalid_tunnel_domain`; a pattern is how a near-miss domain gets admitted. The list is a recorded exfiltration risk and empty is the safe value.
         *
         *     Ceilings are refused, never clamped, when they exceed what this installation permits: a silently narrowed pod spec is one nobody reviewed. A name another profile already holds is refused with `profile_name_taken`. For an injected-runtime first-party profile, a digest that already has a recorded pass is not probed again. A missing or failed digest persists the profile with `conformance_status` pending and runs the in-cluster probe in the background. The profile view flips to passed or failed when the probe records a result. Completed contract failures remain visible on the closed profile by their named codes.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        post: operations["createSandboxProfile"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-profiles/{profile_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Replace a sandbox profile's specification
         * @description Replaces the whole specification, including the harness set and the tunnel-domain list. Both are replaced rather than merged: each is the whole of a security-relevant decision, and a merge would leave the intent to *remove* one expressible only by deleting the profile.
         *
         *     The name is not updatable and is absent from this body. Clients select a profile by name, so a rename silently repoints every spawn call already written against it at a specification its author never reviewed. Author a second profile with `createSandboxProfile` and retire the first with `deleteSandboxProfile`.
         *
         *     Sandboxes already running are unaffected — their ceilings were resolved at spawn and are stored per sandbox, so an edit here cannot retroactively change what a live sandbox was authorized to consume.
         *
         *     The in-cluster image probe runs only when the image, runtime class, placement, or resource requests and limits change. A harness-only or tunnel-only replacement does not start a probe. A digest that already has a recorded pass is not probed again. A missing or failed digest persists the replacement with `conformance_status` pending and runs the probe in the background.
         *
         *     Every rule `createSandboxProfile` applies applies here, including `image_not_digest_pinned`, `runtime_class_required`, and `invalid_tunnel_domain`. Self-administrable like creation (ADR 0040).
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        put: operations["updateSandboxProfile"];
        post?: never;
        /**
         * Retire a sandbox profile
         * @description Removes the profile from every listing and from the set any endpoint's clients may select. The row itself is retained so historical sandboxes, audit entries, and usage attribution still resolve through it — a sandbox whose profile cannot be named after the fact is a sandbox nobody can explain.
         *
         *     The name is not released while the row is retained, so a later profile cannot answer an old spawn call with a different specification.
         *
         *     `setSandboxProfileEnabled` is the reversible way to take a profile out of service. Reach for this one when the specification should never be used again. Sandboxes already running are unaffected; stop one with `cancelSandbox`.
         *
         *     Self-administrable like every other profile write (ADR 0040).
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        delete: operations["deleteSandboxProfile"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-profiles/{profile_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Open a sandbox profile for spawning, or close it
         * @description Closing a profile stops new sandboxes being spawned from it while keeping its specification and its endpoint attachments — the reversible way to withdraw a profile while it is being reconsidered. `deleteSandboxProfile` is the other, and it is not reversible.
         *
         *     Sandboxes already running from the profile keep running: their ceilings were resolved at spawn and the reconciler owns their lifetime. Stop one with `cancelSandbox`.
         *
         *     A profile is created closed, so this is the operation that puts one into service. Self-administrable (ADR 0040): opening a profile is part of the loop that lets an administering model put a specification into service and watch what it produces.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        put: operations["setSandboxProfileEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-runner-classes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List runner classes
         * @description A runner class is an operator-owned size (ADR 0109): CPU, memory, and ephemeral storage, an optional wall-clock default, an optional node selector naming the pool that has machines of this size, and a rank that orders classes from smallest to largest. A profile lists the classes it permits; a spawn picks one. The class carries no harness, image, runtime class, app, repository, model route, or egress entry — nothing on a class weakens isolation.
         *
         *     `profile_names` lists the live profiles currently permitting each class. A class a profile still permits cannot be deleted.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled` rather than an empty list.
         */
        get: operations["listSandboxRunnerClasses"];
        put?: never;
        /**
         * Author a runner class
         * @description A class carries exactly the size fields and nothing that weakens isolation (ADR 0109): no harness, image, runtime class, app, repository, model route, or egress entry. A profile references classes by name; a spawn picks one from the profile's list.
         *
         *     Sizes are validated against the installation's `SandboxBounds` and refused rather than clamped, exactly as a profile is. The rank is unique across the catalogue so escalation has one next class.
         *
         *     A name another class already holds is refused with `runner_class_name_taken`; a rank another class holds with `runner_class_rank_taken`.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        post: operations["createSandboxRunnerClass"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandbox-runner-classes/{runner_class_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Replace a runner class's specification
         * @description Replaces the whole specification. The name is deliberately not updatable: profiles and spawns select a class by name, so a rename would silently repoint every reference at a size nobody reviewed.
         *
         *     Sandboxes already running are unaffected — their ceilings were snapshotted onto the row at spawn, and the pod is rendered from the row. An edit here changes what the next spawn of every profile listing this class gets.
         *
         *     A rank another class already holds is refused with `runner_class_rank_taken`. Self-administrable like profile writes (ADR 0040).
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        put: operations["updateSandboxRunnerClass"];
        post?: never;
        /**
         * Delete a runner class
         * @description A class a live profile still permits is refused with `runner_class_in_use`; detach it from every profile first, with `updateSandboxProfile`. References held by retired profiles are cleared by the delete itself.
         *
         *     Sandboxes already running from the class are unaffected: their ceilings were snapshotted onto the row at spawn.
         *
         *     Self-administrable like every other profile-adjacent write (ADR 0040). An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        delete: operations["deleteSandboxRunnerClass"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List gateway-hosted sandboxes
         * @description Sandboxes newest first, with their lifecycle state, resolved ceilings, and the lineage the gateway bound at spawn — the spawning conversation, run, and execution. That lineage is server-recorded: the workload asserts none of it.
         *
         *     Administrators list installation-wide. Every other caller lists only their own sandboxes, and asking for another user's with `user_id` is refused rather than quietly narrowed — a caller that believes it read one person's sandboxes and actually read its own has been misled by the response it got.
         *
         *     `limit` above 200 is refused rather than clamped, for the same reason. Read one sandbox's event stream with `getSandbox`.
         *
         *     Filter the lifecycle with `states`, a comma-separated set — `states=pending,provisioning,running,completing` is every sandbox still live. The singular `state` remains as the one-value alias; naming both is refused as `conflicting_state_filter` rather than resolved by precedence. A value outside the vocabulary is `invalid_state`, a repeat is `duplicate_state`, and a list naming no state at all is `invalid_state_count`. None of them answers an empty page, which would read as no sandbox being in those states rather than as a filter this endpoint could not apply.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled` rather than an empty list: no sandbox has ever existed here, which is a different fact from none existing now.
         */
        get: operations["listSandboxes"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read one sandbox and its recent events
         * @description The same view `listSandboxes` returns, plus durable progress events in sequence order. With no cursor this preserves the original recent-tail behavior. Set `after_seq` to resume from any sequence and `limit` to page forward through history without gaps. Those rows are the source of truth: the sequence is per sandbox, monotonic, and gap-free, so a client that has seen sequence N knows it has seen everything up to N. Poll this operation and compare `latest_event_seq` to decide whether to read again.
         *
         *     `events_truncated` is set when earlier events exist that this response does not carry; `events_from_seq` is the lowest sequence it does.
         *
         *     Administrators read any sandbox; every other caller reads only their own, and someone else's answers `404` rather than confirming the ID exists.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        get: operations["getSandbox"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/app-http": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List app HTTP attributed to one sandbox
         * @description Sibling of `getSandbox`. Returns governed app requests whose `sandbox_id` is this sandbox, newest first — git forge traffic and the forge's intrinsic REST channel. That is how a closed laptop can still see a push or a pull-request call.
         *
         *     Administrators read any sandbox; every other caller reads only their own, and someone else's answers `404`. An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        get: operations["listSandboxAppHttp"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/cancel": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Cancel one running sandbox
         * @description Moves the sandbox to `cancelled` and appends a sequenced `administrator_cancelled` event to its stream, so a client resuming from `after_seq` learns why the sandbox stopped rather than watching it fall silent. An optional body carries `reason`, a few words recorded as `termination_detail` beside the requester, so the record says whether the run was killed or merely no longer needed.
         *
         *     The row is the desired state and the reconciler acts on it: the backend object is terminated on the next reconciliation cycle, not synchronously here. A `204` therefore means the decision is recorded and irreversible, not that the pod is already gone — read `getSandbox` to watch the backend catch up. A running sandbox keeps its delegated credential for the same slack ceiling deaths already get, so the terminal WIP checkpoint can land; a pending sandbox stops immediately.
         *
         *     **Cancellation is deliberately not model-reachable.** This operation is excluded from the built-in gateway app's catalog, so no model administering this installation through itself can call it. A human cancels a run from the SPA's sandboxes view or with a direct API call carrying their own token. The reasoning is `sendSandboxMessage`'s, pointed at the end of a run rather than the middle of one: terminating a detached agent destroys hours of in-flight work and leaves only the event stream behind, and there is no resume. Reading a sandbox is still model-reachable — `listSandboxes`, `getSandbox`, and `getSandboxConcurrency` answer "what is running and why" and destroy nothing.
         *
         *     A sandbox that has already reached a terminal state is refused with `sandbox_already_terminal`. There is no resume, so cancelling a finished sandbox could only rewrite history.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        post: operations["cancelSandbox"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/egress": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List sidecar egress records for one sandbox
         * @description Sibling of `listSandboxAppHttp`. That read is the gateway-brokered app traffic the broker observed itself; this one is the registry egress the sidecar observed and reported durably (ADR 0028/0072): blind tunnel connections with byte counts, per-request records for intercepted domains (method, host, path, status — never bodies, never headers), and refused destinations. Newest first, so "what did this sandbox fetch" stays answerable after the pod and its log capture are gone.
         *
         *     Administrators read any sandbox; every other caller reads only their own, and someone else's answers `404`. An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        get: operations["listSandboxEgress"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/inference": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List inference attributed to one sandbox
         * @description Sibling of `getSandbox`. Returns the inference events whose CLI session is this sandbox, newest first, so the watch surface can show spend and model use on the timeline instead of only under Usage. `getSandbox` stays a sandbox document.
         *
         *     Administrators read any sandbox; every other caller reads only their own, and someone else's answers `404`. An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        get: operations["listSandboxInference"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/logs": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read a sandbox's container output
         * @description Returns container stdout/stderr as framed, redacted, bounded untrusted data (ADR 0040 D). The response always names `content_kind` `untrusted_container_output` so a consumer cannot treat the body as instruction.
         *
         *     **Snapshot (default).** Omitting `follow` returns a JSON object with one entry per container (or the one named by `container`). `tail` keeps only that many recent lines per container. When a container is cut by `sandboxes.output_tail_kib`, `truncated` is true and `cursor` continues that container without replaying the others. This is what a detached-agent post-mortem uses when the pod is still up, and what pairs with the durable `container_output` event written before terminate deletes the pod.
         *
         *     **Live tail (`follow=true`).** Streams one container as `text/event-stream`. Each `container_output` event carries one framed line; a final `stream_end` event names why the stream stopped (`bound_reached`, `completed`, `unavailable`). Defaults `container` to `workload` when omitted. `tail` sets how many historical lines the stream replays. The stream is cut at `sandboxes.output_tail_kib` so a workload cannot flood an administrator's context by design.
         *
         *     Not withheld: reading what a profile produced is exactly what an administering model should have.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`. One whose log capture bound is zero answers with an empty snapshot or an immediate `stream_end`.
         */
        get: operations["getSandboxContainerLogs"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/sandboxes/{sandbox_id}/messages": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Send a steering message to a running sandbox
         * @description Appends a sequenced message to the sandbox's inbox. Its supervisor drains the inbox on its own loop and hands the message to the harness as ordinary input — the gateway attaches no meaning to the body, and there is never a network path between this caller and the pod.
         *
         *     Delivery is at-least-once and asynchronous. A `200` means the message is durable and ordered, never that the agent has read it: the supervisor advances its cursor only after handing a message over, so a supervisor that restarts mid-drain redelivers rather than skips. `pending_messages` in the response is what is still ahead of the agent.
         *
         *     Set `interrupt` to preempt the turn in flight instead of waiting for it to end. Leave it false unless the current turn must stop: an interrupted turn leaves the workspace mid-edit and a finished one does not.
         *
         *     **Only the owner may steer.** Unlike `cancelSandbox`, this is not available to an administrator acting on someone else's sandbox, and the grants that authorized the spawn are re-evaluated here rather than inherited — a user whose team, app, or endpoint access was withdrawn after spawning is refused even though their session is still valid. Someone else's sandbox answers `404` rather than confirming the ID exists. An administrator who needs a detached run to stop calls `cancelSandbox`; there is no administrative back door into an agent's input, because a message is indistinguishable from the owner's own words once it reaches the harness.
         *
         *     Excluded from the built-in gateway app's catalog. Steering a detached agent is the blast radius ADR 0024 argues about applied to a running process: a model administering this installation through itself could otherwise put words into another agent's input that arrive as the owner's own, and every later action of that run would be attributed to a user who never typed them. A human with a token calls this exactly as before.
         *
         *     A sandbox that has stopped is refused with `sandbox_already_terminal`. An inbox with the maximum undelivered messages waiting is refused with `sandbox_inbox_full`, which clears when the supervisor drains.
         *
         *     An installation that does not run sandboxes answers `409` with `sandboxes_not_enabled`.
         */
        post: operations["sendSandboxMessage"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/scim-connectors": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List SCIM connectors
         * @description Each connector is one directory's bearer credential for provisioning accounts and groups into this installation. The token itself is shown once at creation and never again; `token_prefix` is the non-secret leading characters, enough to tell which credential a directory is presenting.
         *
         *     Retired connectors are listed too, with `enabled` false. `last_used_at` is the last successful authentication, which is how to tell a connector that is configured somewhere from one that was created and never wired up.
         */
        get: operations["listScimConnectors"];
        put?: never;
        /**
         * Create a SCIM connector and mint its bearer token
         * @description Mints one directory's provisioning credential. The `token` in the reply is the only time it exists in readable form: this gateway keeps a digest, so it cannot show the token again and cannot recover one that was not kept. A caller that discards it has to create another connector and retire this one.
         *
         *     Configure the directory with the returned `scim_base_url` and `token`. `token_prefix` is the non-secret leading characters, which is how `listScimConnectors` identifies which credential a directory is presenting without disclosing it.
         *
         *     The connector authenticates from the moment it is created — there is no disabled state to promote it out of, unlike an identity provider. A reused name is refused with `name_taken`, and nothing is stored: the token minted for that attempt authenticates nothing and should be discarded rather than retried. A name that is empty or longer than 120 characters is `invalid_name`. The creation is recorded in the audit ledger without the bearer token or its digest.
         */
        post: operations["createScimConnector"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/scim-connectors/{connector_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Retire a SCIM connector
         * @description Only `false` is accepted. `true` is refused with `connector_reactivation_unsupported`, and the reason is not that re-enabling is unimplemented: a retired connector keeps its bearer token, so putting it back into service restores a credential that was withdrawn, for whoever still holds a copy of it. Create a new connector instead and configure the directory with the new token — that is the operation that mints a credential nobody has yet.
         *
         *     Retiring stops the connector authenticating. It does not remove the accounts or groups it provisioned: those stay, and stop being updated from that directory. Deactivate the accounts themselves with `updatePerson` if that is what is meant.
         */
        put: operations["setScimConnectorEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/service-identities": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List service identities
         * @description Every non-human account, with its owning team. Administrator-only.
         */
        get: operations["listServiceIdentities"];
        put?: never;
        /**
         * Create a service identity
         * @description Creates a non-human account and places it in its owning team. The identity holds no login identifiers and can never be an administrator; the schema forbids both. Grant models to the owning team — team attribution is what carries its spend into team rollups and team limits. Connected apps reach it through the owning team's grants or, for an app that is the identity's own presence on another system such as a GitHub App in installation mode, through `setServiceIdentityAppGrant`. Sessions come only from the client-credentials grant of the registered add-on bound to this username.
         */
        post: operations["createServiceIdentity"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/service-identities/{user_id}/apps/{app_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Grant or revoke a service identity's own connected app
         * @description Gives the identity a connected app directly, independent of its owning team, or takes that grant away. Meant for an app that is the identity's own presence on another system — a GitHub App in installation mode that reviews as the bot — while shared connectors stay on the team where people can see them. Reachability is the union of both grants; where exactly one forge app must serve an origin (a repository spawn, a hosted-machine credential mint), the directly held app outranks every team-inherited one. Granting is refused with `forge_origin_collision` (409) when the identity would hold two forge apps for one origin. Attribution still follows the owning team. Revoking sweeps the identity's live sandboxes that reached the app only through this grant.
         */
        post: operations["setServiceIdentityAppGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/service-identities/{user_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Enable or disable a service identity
         * @description Disabling is the kill switch: the same transaction revokes every session and asks every live sandbox of the identity to stop. Re-enabling restores nothing by itself — the add-on mints a fresh session on its next request.
         */
        post: operations["setServiceIdentityEnabled"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/service-identities/{user_id}/team": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Move a service identity to a new owning team
         * @description Replaces the identity's owning team. Membership is the identity's ownership, its team-inherited app access, and its spend-attribution container, and an identity holds exactly one — the old membership is removed in the same transaction. Use this to adopt an identity orphaned by a team deletion. Leaving the old team can revoke team-inherited app access a live sandbox depended on; those sandboxes are asked to stop. Apps the identity holds directly (`setServiceIdentityAppGrant`) move with it untouched.
         */
        post: operations["setServiceIdentityTeam"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List the shared apps you can reach
         * @description Every shared app the calling user reaches: their own — drafts, published, and stopped ones they have yet to delete — plus apps published to a team they belong to. A stopped app is listed for its author alone, and reading it is not being able to run it. Reachability is the same rule the shell page and the invoke path use, so for an ordinary caller this listing is exactly the set of apps they could open.
         *
         *     Gateway administrators see all of that **plus** every published or stopped app in the installation, because `disableSharedApp` gives them an artifact-level stop and an authority that cannot find the artifact is not one. It is added to what they already reach rather than substituted for it, so an administrator's own drafts stay in their listing like anyone else's. Somebody else's never-published draft stays out — a draft reaches its author and nobody else, so there is no audience to stop and no incident to review, and a shared app is a user's artifact that the administrator role is not by itself a reason to read. Stopped apps stay listed, because reviewing what was stopped is the other half of holding the stop. What widens is metadata and, through `getSharedApp`, the manifest — what an app may call. Authored bytes never do. Being listed here is still not being able to run it: an administrator reaches no invoke they were not already entitled to.
         *
         *     Manifests are not included. Read one app with `getSharedApp` to see what it may call.
         */
        get: operations["listSharedApps"];
        put?: never;
        /**
         * Register a shared app as a draft
         * @description Creates the app as a `draft` with revision 1. Draft is not a staging step, it is the authoring surface: the app runs as its viewer from its first call, with the author as its only reachable viewer. `publishSharedApp` changes who reaches it, not how it runs.
         *
         *     `slug` is unique per author rather than installation-wide, and is derived from `name` when omitted. A name with nothing sluggable in it is `invalid_slug`; a slug this author already used is `slug_taken`.
         *
         *     The manifest is the security object and is validated here, against the live catalog, as the calling user. `app_not_entitled` means a bound app is not one the caller reaches in their own right, or is disabled — the same answer either way, because an author must not learn that an app they cannot reach exists. `built_in_app_not_bindable` refuses the app this gateway provisions for itself: self-administration from an authored page is the blast radius ADR 0024 kept off the Direct tier. `agent_access_unsupported` means the bound app does not expose the structured Proxy API — a shared-app call is a single structured operation, so a `governed_shell`-only app is not bindable and `setConnectedAppAgentAccess` is what would change that. `unknown_operation` names the exact ids missing from that app's ingested catalog; resolve them against the app's operations rather than guessing. `duplicate_binding` means one connected app was named twice, which would store a manifest that does not match what was sent.
         *
         *     Two whole regions of the manifest are refused rather than ignored. `server_sections_unsupported` refuses a `server` section: v1 ships client-bundle-only. `unknown_operation_policy` refuses every key under `operation_policies`, which is reserved space — an older gateway silently dropping a policy a newer manifest relies on is exactly the failure the reservation exists to prevent.
         *
         *     `manifest_too_large`, `invalid_bundle`, and `bundle_too_large` bound what is stored: manifests up to 32 KiB compact, bundles up to 1 MiB decoded. The bundle is stored as sent and never parsed. `invalid_manifest` covers everything else about the manifest's own shape, including a member v1 does not define, and `invalid_name` covers the app's own name and the optional `client_name` provenance label — both are refused rather than trimmed to fit.
         */
        post: operations["createSharedApp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps/{shared_app_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read one shared app and the manifest it pins
         * @description Returns the app's metadata and the manifest of the revision it currently serves — what the app may call, and as whom. The bundle is not returned; only its size is, because the manifest is the part that is reviewable and the bundle is authored content the gateway does not inspect. That holds for administrators too: the read adds every published or stopped app to what they already reach, so an administrator can see what one they were asked to stop is permitted to do; it stops at the manifest, and it does not reach somebody else's never-published draft.
         *
         *     An app the caller does not reach is `404`, whether or not it exists.
         *
         *     `granted_teams` lists the audience, and is present only for the author and for an administrator — the principals who can already change it. It is absent, rather than empty, for a granted viewer: which other teams hold an app is not something reaching it entitles you to learn. Present-and-empty means the app is published to nobody. A team appears here while disabled, carrying `enabled: false`, because the grant exists and reaches nobody.
         */
        get: operations["getSharedApp"];
        put?: never;
        post?: never;
        /**
         * Delete a shared app
         * @description Retires the app and every grant on it. The revisions and the ledger rows that cite them survive, because a deleted app must not take the record of what it did with it. The slug is released: it is unique among an author's live apps, so the same name can be used again.
         *
         *     Only the author may delete, and they may delete a stopped app — disabling is terminal, and deletion is what it leads to. A caller who can read the app but does not own it is `shared_app_authority_required`; one who cannot reach it is `404`. `disableSharedApp` is the way to stop an app without destroying it, and the one a team's managers can reach.
         */
        delete: operations["deleteSharedApp"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps/{shared_app_id}/disable": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Stop a shared app for everyone who was granted it
         * @description Disabling is the **safety stop**, not an audience edit. It makes the app unreachable and uncallable by everyone it was published to, and uncallable by its author too: nobody invokes a stopped app, and `publishSharedApp` refuses to restart one. Disabling is terminal in v1 — the only thing left to do with a stopped app is `deleteSharedApp`. To take one team out of an app's audience while leaving it running for the others, use `revokeSharedAppTeamGrant` instead.
         *
         *     What the author keeps is visibility and deletion: a stopped app still appears in their listing and still reads, so the artifact they own can still be retired. An app its own author could no longer see would be one nobody could ever delete, holding its slug and its grants forever — a leak wearing a stop button's clothes.
         *
         *     Three authorities can call it, and the audit event's actor says which one did: the author; a manager of a team the app was published to — this is where `manager` team membership acquires authority, so a team that was granted an app can stop it without its author; and a gateway administrator, whose alternative is otherwise to disable a whole team or a whole connected app to stop one page. Anyone else who can read the app is `shared_app_authority_required`; anyone who cannot reach it is `404`. A second call on an already-stopped app is `204` for its author and for an administrator, and `404` for a manager whose team can no longer reach it.
         *
         *     Nothing is destroyed. `deleteSharedApp` is the irreversible one.
         */
        post: operations["disableSharedApp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps/{shared_app_id}/publish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Publish a shared app to a team you belong to
         * @description Publishing is granting: the app's status becomes `published` and the named team is granted reachability. It changes who may open the app, never what the app may do — every call it makes is still authorized against the *viewer's* own entitlement to the underlying connected app, so a grant here cannot carry the author's access to anyone.
         *
         *     Only the author may publish, and only to a team they belong to: a team the caller is not a live member of is `not_a_team_member`. A caller who can read the app but does not own it is `shared_app_authority_required`.
         *
         *     The current manifest is re-validated against the live catalog first, with the same refusals `createSharedApp` documents. This is a hygiene gate rather than the security boundary — entitlement is re-checked per viewer on every call — but publishing an app that is already broken for its author would put a page in a team's path that cannot work for anyone. `host_local_bridge_verbs` is the same kind of gate on the bundle: a page calling a harness's filesystem verbs has no counterpart here and would render sections that never arrive, so it is refused with the exact verbs to remove. Drafts are not held to it — that is where a conversion from harness-local bindings happens.
         *
         *     An app a manager stopped is `app_disabled` and stays stopped. Re-publishing over `disableSharedApp` would make a team's stop button a suggestion, so disabling is terminal in v1: the author keeps reading and deleting the app, and nothing re-publishes it. Publishing again after `revokeSharedAppTeamGrant` is a different matter and is allowed: revocation edits the audience, and re-granting a team is this same call.
         *
         *     The bundle is author-provided content the gateway does not inspect. Data an author baked into those bytes travels to every viewer, invisibly to both the manifest and the invoke ledger; clearing host-local source material before publishing is the author's to do.
         */
        post: operations["publishSharedApp"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps/{shared_app_id}/revisions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Update a shared app by appending an immutable revision
         * @description Updating a shared app means adding a revision, never editing one. The new revision becomes what the app serves, and the previous manifest stays readable — which is what lets a ledger row answer "which manifest authorized this call" after the app has moved on.
         *
         *     Only the author may append. A caller who can read the app but does not own it is `shared_app_authority_required`; one who cannot reach it at all is `404`.
         *
         *     The manifest is re-validated in full, against the live catalog, as the calling user — the same refusals `createSharedApp` documents. An app that was bindable when revision 1 was written and has since been narrowed is refused here rather than silently kept.
         *
         *     An app a manager stopped is `app_disabled`. A stopped app is readable and deletable by its author, and nothing else: appending to one would let an author keep working on an artifact a team has stopped, and would leave the revision waiting to become live if disabling were ever made reversible.
         *
         *     Old revisions are pruned past a retention cap, except any that the app still points at, a ledger row cites, or a consent depends on.
         *
         *     Appending to a **published** app is held to the bundle rule publishing applies: a revision calling a harness's filesystem verbs is refused with `host_local_bridge_verbs` naming them. The new revision is what every viewer is served the moment it lands, so the gate has to be here as well as at publish. Drafts are exempt — that is where a conversion from harness-local bindings happens, and nothing a draft carries is servable to a team until a publish judges it.
         */
        post: operations["createSharedAppRevision"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/shared-apps/{shared_app_id}/teams/{team_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Take one team out of a shared app's audience
         * @description Revoking is an **audience edit**, not a stop. The named team loses reachability; the app keeps running for every other team it was published to, and for its author. `disableSharedApp` is the safety stop, and it is the one to reach for when an app should not be running at all.
         *
         *     Two authorities can call it, and they are deliberately different sizes. The **author** may revoke any grant on their own app — it is their artifact and its audience is theirs to edit. A **manager** may revoke exactly their own team's grant: a stated "not for my team", which is the authority ADR 0036 gives team managers over what reaches their people, and it requires the team to be enabled and to actually hold the grant. A manager of a disabled team cannot withdraw it — the same predicate `disableSharedApp` uses, and for the same reason: a team that confers nothing is not currently governing anything. There is no administrator arm. An administrator's concern is the artifact, and `disableSharedApp` is where they act on it — so although the widened administrator read now shows them every published or stopped app, calling this on one they neither authored nor manage a granted team of is `404`, the same answer any other outsider gets. Being able to read an app is not being able to edit whose it is.
         *
         *     Anyone else who can read the app is `shared_app_authority_required`, and a caller who cannot reach the app at all is `404`.
         *
         *     Revoking the last grant leaves the app `published` with an empty audience — reachable by its author alone. The status does not snap back to `draft`: `published` records that the author completed the publish ritual, including its authored-content acknowledgment, and grants alone carry the audience. Re-granting the team is `publishSharedApp`, and it is allowed after a manager's revoke — the `shared_apps.grant_updated` trail names both actors, so a standing disagreement inside a team is visible rather than silently arbitrated here.
         *
         *     Viewers' consent rows survive. Consent records what somebody accepted this app may call as them; a grant records who can reach it. Deleting the record of a decision that was really made would be a different thing than withdrawing access.
         *
         *     Idempotent for the **author** only: a team of theirs that held no grant is `204` and writes nothing, because the audit event records an audience changing rather than a request arriving. A manager repeating the call is refused instead, and that is the authority rule rather than an inconsistency — their authority *is* the live grant, so once it is gone there is nothing left for them to be a manager of. Both answers say the grant is not there.
         */
        delete: operations["revokeSharedAppTeamGrant"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** List teams */
        get: operations["listTeams"];
        put?: never;
        /** Create a team */
        post: operations["createTeam"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * Delete a team
         * @description Deletes the team, its memberships, and every grant it carried. Not reversible and not the same as disabling it: the grants are gone rather than suspended, so restoring the access afterwards means granting it again from scratch. Use `setTeamEnabled` when the access should stop but the team should survive.
         *
         *     The people keep their accounts and any access they hold through another team. Recorded inference events survive with the team they were attributed to at the time.
         *
         *     Answers 404 for an ID that names nothing, unlike the admin page, which lands on the team list either way — a caller told 204 for an ID it typoed would record that a team it never touched is gone.
         */
        delete: operations["deleteTeam"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}/apps": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Grant or revoke a connected app for a team
         * @description Team grants do not apply to the built-in Model Gateway app, whose reachability is its audience setting; granting it is rejected.
         */
        post: operations["setTeamAppGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}/enabled": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Enable or disable a team
         * @description Disabling a team suspends every grant it carries without deleting any of them: its members stop reaching the models and apps they reached through it, and enabling the team restores exactly what it held before. This is the reversible half of `deleteTeam`, and the one to use when access should stop but the membership and grants are still wanted.
         *
         *     A member who also reaches a model through another team, or through a grant of their own, keeps that access — this suspends one team's grants, not a person's access.
         */
        put: operations["setTeamEnabled"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}/members": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * List team members
         * @description Returns the user and role for every current membership in the team. Use this after `setTeamMembership` to verify the resulting state; an unchanged `member_count` alone cannot identify which user holds a membership.
         */
        get: operations["listTeamMembers"];
        put?: never;
        /**
         * Set or remove a team membership
         * @description Set `membership` to `member` or `manager` to place a user in the team, or to `remove` to remove them. The action is required; omitting it is rejected rather than treated as removal. Use `listPeople` to resolve an email to a `user_id`, and `listTeamMembers` to verify the resulting state.
         */
        post: operations["setTeamMembership"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}/models": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * Grant or revoke a model for a team
         * @description Granting a model to a team is what lets its members route to that model. A model that exists but is granted to nobody is unreachable.
         */
        post: operations["setTeamModelGrant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/teams/{team_id}/name": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * Rename a team
         * @description Changes the team's display name. The slug does not move: usage history and recorded inference quote it, so a caller that stored `platform-engineering` keeps resolving the same team after a rename. There is no operation to change a slug — create a team with the slug you want.
         *
         *     Names are unique case-insensitively, so a rename onto another team's name is refused rather than leaving two teams a person cannot tell apart.
         */
        put: operations["setTeamName"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/api/v1/admin/usage": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Read usage rollups
         * @description Aggregates every recorded inference, tool call, and governed app request into five views of the same events: by account, by team, by model, by authenticating client, and by connected app. A request appears in more than one of them; the views are cuts, not partitions, and summing across them double-counts.
         *
         *     An administrator reads the installation; every other caller reads only their own traffic. The `scope` field says which, because the two responses are otherwise indistinguishable.
         *
         *     Costs are integer micro-US-dollars — 1 000 000 to the dollar — and stay partitioned by payer. `estimated_cost_microusd` is metered spend, `subscription_cost_microusd` is usage a user's subscription absorbed, `provisioned_cost_microusd` is usage provider-provisioned capacity absorbed, and `credits_cost_microusd` is prepaid-credit draw-down. Where `priced_requests` is below `inference_requests` some request had no rate to price it, and the cost is a floor rather than a total; `getCostStateSummary` reports why. Never sum the four cost fields without preserving their different payers.
         *
         *     Every inference rollup also reports prompt-cache visibility: `cached_input_tokens` (input served from a provider cache), `cache_write_input_tokens` (input written to one), and `cache_savings_microusd` — the estimated metered spend caching avoided versus billing every input token at each event's snapshotted base input rate, i.e. the cache-read discount minus the cache-write premium. Savings counts billed rows only, so it pairs with `estimated_cost_microusd`; it is floored so a saving is never overstated, and it is negative when write premiums exceeded read savings.
         *
         *     Filter inference traffic with `user_id`, `team_id`, `model_id`, `client_name`, `sandbox_id`, `repo_slug`, and an inclusive `since` / exclusive `until` window. Add `group_by` with one to three dimensions (`user`, `team`, `model`, `client`, `day`, `sandbox`, `repo`, `execution_kind`) for cross-tabulation. Filter names (`user_id`, `team_id`, `model_id`, `client_name`, `sandbox_id`, `repo_slug`) are accepted as `group_by` aliases. Filtered requests return up to 1,000 matching inference aggregates in `grouped`; `grouped_truncated` says whether more matched.
         *
         *     Unfiltered and window-only reads also include `summary`: the authoritative inference, tool, and app totals for that scope and window. A dimension filter or `group_by` omits it, because those summaries cannot apply those cuts; use `grouped` instead.
         *
         *     Any filter or grouping answers in `grouped` rather than in the rollup arrays, which are then empty — the two are alternatives, not a merged view. `grouped` carries the same committed-attempt efficiency statistics the by-model and by-client rollups report (`committed_requests`, `abandoned_requests`, average / median / 95th-percentile duration and time-to-first-byte, peak context utilization, and mean message and tool-definition counts), so narrowing a question to a window, a client, or a repository does not cost the answer: p95 first-byte for one client over the last seven days is `group_by=client` with `since`. Those statistics exclude abandoned failover attempts — a retried attempt's duration is nobody's wait — while token and cost sums in `grouped` count every attempt; `committed_requests` is the visible denominator.
         *
         *     `repo` groups by the slug recorded on each event, which is what spend was incurred under, not the project a conversation was last resumed in. Events with no slug group as one null-keyed Unlabeled row; `repo_slug` filters for one exact slug and cannot select that remainder. The unfiltered compatibility view includes `by_sandbox` and `by_execution_kind` so agent spend is never blended into a workstation client bucket. Unknown parameters are refused rather than silently ignored.
         */
        get: operations["listUsage"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        /**
         * @description What one access-policy statement does to the operations it names.
         * @enum {string}
         */
        AccessPolicyEffect: "allow" | "deny";
        /**
         * @description Whose requests one access-policy statement covers.
         * @enum {string}
         */
        AccessPolicyScopeKind: "app" | "team" | "user" | "sandbox_profile";
        AddOnCreateRequest: {
            capability_allowlist?: unknown;
            display_name: string;
            enabled?: boolean;
            kind?: string | null;
            /**
             * @description Create an instance of a registered preset (ADR 0110): `shipright`,
             *     `slack-adapter`, or `tidebreak`. The registry supplies kind, resource
             *     family, capabilities, and the settings schema; the request supplies
             *     the slug, the display name, and the service identity it mints for.
             *     Omit for a custom add-on.
             */
            preset?: string | null;
            resource_family?: string | null;
            service_mint_username?: string | null;
            slug: string;
            stability_tier?: string | null;
        };
        /**
         * @description Environment variables to write into or remove from a managed add-on's
         *     workload. The plane holds the values write-only: a response names what is
         *     set, never what it is set to.
         */
        AddOnEnvironmentUpdateRequest: {
            /** @description Variables to remove. A name the workload does not hold is ignored. */
            unset?: string[];
            /**
             * @description Variables to set. A name already present is overwritten; every other
             *     variable the workload holds is left alone.
             */
            values?: {
                [key: string]: string;
            };
        };
        /**
         * @description Latest per-add-on reachability facts.
         *
         *     Three independent fact pairs — traffic success, traffic failure, probe —
         *     each answering a different question; readers compare the timestamps to
         *     decide which evidence is current. Gateway refusals never appear here.
         */
        AddOnHealthView: {
            /** @description When authentication last failed in a way attributable to the add-on. */
            last_failure_at?: string | null;
            /** @description `unreachable`, `auth_rejected`, or `upstream_error`. */
            last_failure_code?: string | null;
            /**
             * Format: int32
             * @description HTTP status of that failure, when a response was received at all.
             */
            last_failure_status?: number | null;
            /** @description When an admin probe last ran. */
            last_probe_at?: string | null;
            /**
             * @description Coarse probe classification (`http_<status>`, `dns`, `connect`,
             *     `timeout`, `other`); never upstream prose.
             */
            last_probe_detail?: string | null;
            /** @description Whether that probe got any HTTP response. */
            last_probe_ok?: boolean | null;
            /** @description When a machine last authenticated successfully. */
            last_success_at?: string | null;
            /**
             * Format: int32
             * @description HTTP status of that success, when one exists.
             */
            last_success_status?: number | null;
        };
        /**
         * @description Desired hosting state for one add-on (ADR 0095).
         *
         *     Writing it makes the row managed; every field is required because the
         *     database's hosting-shape constraint takes the whole desired state or none
         *     of it. The status trio is absent on purpose — the reconciler owns it. The
         *     hostname override is the operator's bring-your-own name (ADR 0095
         *     decision 4); a write that omits it clears it, so the row falls back to the
         *     hostname the reconciler assigned — which no write here ever touches.
         */
        AddOnHostingUpdateRequest: {
            /**
             * @description Where the workload's database comes from (ADR 0095 decision 5).
             *     `none` — the add-on runs without a database — `external` — an
             *     operator-supplied Secret — `gateway_schema` — a role and schema the
             *     gateway provisions in its own PostgreSQL — and `colocated` — a
             *     disposable PostgreSQL the plane runs beside the workload — are all
             *     accepted.
             */
            database_mode: string;
            /**
             * @description Where the workload may speak beyond the gateway, cluster DNS, and
             *     its database: `gateway` (the default) or `internet`, which adds
             *     outbound HTTPS to public addresses.
             */
            egress?: string | null;
            exposure: string;
            health_path?: string | null;
            hostname_override?: string | null;
            /**
             * @description Required for a pinned row: the one pin. Informational for a tracking
             *     or first-party row — the reconciler owns the column and a write that
             *     keeps what the row follows keeps the adopted digest — so a caller
             *     echoing the view back need not strip it.
             */
            image_digest?: string | null;
            image_reference: string;
            /**
             * @description `pinned` (the default), `track_release`, or `first_party` (ADR
             *     0108): a first-party row follows the image this deployment pinned
             *     for the add-on in `add_ons.hosting.first_party`.
             */
            image_source?: string | null;
            lifecycle: string;
            /** Format: int32 */
            listen_port: number;
            /**
             * @description Pauses adoption when true, on a tracking or first-party row; omitted
             *     means following. Refused on a pinned row.
             */
            release_hold?: boolean | null;
            /**
             * @description The GitHub repository (`owner/name`) a tracking row follows.
             *     Required for `track_release`, refused otherwise.
             */
            release_repository?: string | null;
            /**
             * Format: int32
             * @description The row's own soak window, in minutes from 0 through 10080. Only for
             *     `track_release`; omitted takes the installation default.
             */
            release_soak_minutes?: number | null;
            /**
             * @description Names of `add_ons.hosting.source_sets` entries restricting a public
             *     add-on's ingress. Omit to leave a public add-on unrestricted; internal
             *     exposure refuses the field, being already the private-ranges
             *     restriction.
             */
            source_sets?: string[] | null;
            /**
             * @description The durable volume the plane owns for this workload (ADR 0095
             *     decision 2, amended): mounted at `volume_mount_path`, sized
             *     `volume_size` (a Kubernetes quantity such as `20Gi`), on an optional
             *     `volume_storage_class`. Omit all three for no volume.
             */
            volume_mount_path?: string | null;
            volume_size?: string | null;
            volume_storage_class?: string | null;
        };
        /**
         * @description One add-on's hosting state (ADR 0095).
         *
         *     `mode` is always present; every other field is populated only on managed
         *     rows, where the database's hosting-shape constraint guarantees the desired
         *     state and the reconciler owns the status trio.
         */
        AddOnHostingView: {
            /**
             * @description The in-cluster origin another managed add-on reaches this workload at
             *     (plain HTTP to its Service). Present while the row is installed. This
             *     is how the Slack adapter is pointed at a hosted Tidebreak whose
             *     Ingress is internal: the plane admits egress between managed add-ons,
             *     and nothing else in the cluster resolves the private hostname.
             */
            cluster_url?: string | null;
            database_mode?: string | null;
            /**
             * @description `gateway` confines the workload to the gateway, cluster DNS, and its
             *     database; `internet` additionally admits outbound HTTPS to public
             *     addresses.
             */
            egress?: string | null;
            /**
             * @description Digest of the last environment write; the workload rolls when it
             *     changes.
             */
            environment_digest?: string | null;
            /**
             * @description Names of the variables written into the workload's environment Secret.
             *     Names only: the plane holds the values write-only, so no path reads
             *     them back.
             */
            environment_keys?: string[] | null;
            exposure?: string | null;
            health_path?: string | null;
            hostname?: string | null;
            hostname_override?: string | null;
            /**
             * @description The digest the workload converges toward. Absent only on a tracking
             *     row that has not adopted a release yet.
             */
            image_digest?: string | null;
            image_reference?: string | null;
            /**
             * @description Where the digest comes from (ADR 0108): `pinned` — written with the
             *     declaration; `track_release` — resolved by the reconciler from the
             *     newest admitted release of `release_repository`; or `first_party` —
             *     the image this deployment pinned for the add-on, adopted whenever a
             *     deployment moves it.
             */
            image_source?: string | null;
            lifecycle?: string | null;
            /** Format: int32 */
            listen_port?: number | null;
            mode: string;
            preset?: null | components["schemas"]["HostingPresetView"];
            provenance?: null | components["schemas"]["ImageProvenanceView"];
            release_adopted_at?: string | null;
            release_candidate_reason?: string | null;
            /**
             * @description The newest release seen but not adopted, and why — soaking, not yet
             *     published as an image, refused by the trust policy, or held.
             */
            release_candidate_tag?: string | null;
            /** @description When the tracked repository's releases were last read. */
            release_checked_at?: string | null;
            /** @description True pauses tracking: candidates are recorded, nothing is adopted. */
            release_hold?: boolean | null;
            /** @description The GitHub repository (`owner/name`) a tracking row follows. */
            release_repository?: string | null;
            /**
             * Format: int32
             * @description The row's own soak window in minutes; absent takes the installation
             *     default.
             */
            release_soak_minutes?: number | null;
            /** @description The release tag the running digest was adopted from, and when. */
            release_tag?: string | null;
            source_sets?: string[] | null;
            status?: string | null;
            status_reason?: string | null;
            status_updated_at?: string | null;
            volume_mount_path?: string | null;
            volume_size?: string | null;
            volume_storage_class?: string | null;
            /**
             * @description SHA-256 hex of the minted webhook secret, when the row has one. A
             *     digest, never the plaintext: the plaintext exists only in the mint
             *     response and the workload's webhook Secret (ADR 0095 decision 5).
             */
            webhook_secret_digest?: string | null;
        };
        AddOnListResponse: {
            data: components["schemas"]["AddOnView"][];
        };
        /** @description What the gateway relays of an add-on's pairing report. */
        AddOnPairingReportResponse: {
            detail?: string | null;
            /**
             * @description Whether the add-on answered at all. `false` names the transport
             *     failure in `detail`; the report itself may still say a machine is
             *     unpaired while this is `true`.
             */
            reachable: boolean;
            /**
             * @description The add-on's own report, when it answered JSON. A 503 still carries
             *     one: the adapter answers that way while a machine is unpaired.
             */
            report?: unknown;
            /** Format: int32 */
            status?: number | null;
            /** @description Where the report was fetched from. */
            target: string;
        };
        AddOnSettingsResponse: {
            schema: unknown;
            settings: unknown;
        };
        /**
         * @description Enablement is deliberately absent: the kill switch is its own audited
         *     operation (`setAddOnEnabled`), matching every other admin surface.
         */
        AddOnUpdateRequest: {
            capability_allowlist: unknown;
            display_name: string;
            kind: string;
            resource_family: string;
            service_mint_username?: string | null;
            stability_tier: string;
        };
        AddOnView: {
            capability_allowlist: unknown;
            /** @description The OAuth client identity tokens bind to. Stable across renames. */
            client_id: string;
            display_name: string;
            enabled: boolean;
            has_current_secret: boolean;
            has_previous_secret: boolean;
            health?: null | components["schemas"]["AddOnHealthView"];
            hosting_mode: string;
            /** Format: uuid */
            id: string;
            kind: string;
            /**
             * @description The preset this row is an instance of (ADR 0110); null for a custom
             *     add-on. The console groups by it.
             */
            preset?: string | null;
            resource_family: string;
            service_mint_username?: string | null;
            settings: unknown;
            settings_schema?: unknown;
            slug: string;
            stability_tier: string;
        };
        AllowedAddOnImageListResponse: {
            data: components["schemas"]["AllowedAddOnImageView"][];
        };
        /** @description One image trust policy entry (ADR 0095 decision 3). */
        AllowedAddOnImageView: {
            /**
             * @description Digests admitted from the repository by list. Empty on an entry that
             *     admits by provenance.
             */
            digests: string[];
            provenance?: null | components["schemas"]["AllowedImageProvenanceView"];
            /** @description Exact repository — registry host and path, with no tag and no digest. */
            repository: string;
        };
        AllowedImageProvenanceView: {
            /** @description The GitHub repository, as `owner/name`. */
            repository: string;
            /** @description The workflow file path. */
            workflow: string;
        };
        /** @description Error envelope, matching the shape the rest of the gateway returns. */
        ApiErrorBody: {
            /** @description Stable machine-readable error identifier. */
            code: string;
            /** @description Per-field rule failures, present only for validation errors. */
            fields?: components["schemas"]["FieldError"][];
            /** @description Human-readable summary. */
            message: string;
        };
        /** @description Response body for every non-success control-plane result. */
        ApiErrorResponse: {
            /** @description The error. */
            error: components["schemas"]["ApiErrorBody"];
        };
        AppliedGuardrailTemplateResponse: {
            data: components["schemas"]["AppliedGuardrailTemplateView"];
        };
        AppliedGuardrailTemplateView: {
            /**
             * Format: int64
             * @description Which application of this template this was, counting from one. It is
             *     what suffixed the created names and engine keys.
             */
            apply_sequence: number;
            engine_ids: string[];
            policy: components["schemas"]["GuardrailPolicyView"];
        };
        /** @enum {string} */
        AppliesTo: "all" | "sandbox";
        /**
         * @description The apply request.
         *
         *     It carries no `mode` and no `failure_stance`, and that is the mechanism
         *     rather than an omission: apply always creates a monitor-mode, fail-open
         *     policy, and `deny_unknown_fields` turns an attempt to ask for anything else
         *     into a readable refusal instead of a silently ignored field (ADR 0066).
         */
        ApplyGuardrailTemplateRequest: {
            /** @description Declared parameter values, each a list of strings. */
            parameters?: Record<string, never>;
            /** Format: uuid */
            scope_id?: string | null;
            scope_type: components["schemas"]["ScopeType"];
        };
        /** @description Traffic through one connected app. */
        AppUsageView: {
            /**
             * Format: uuid
             * @description Stable connected-app ID.
             */
            app_id: string;
            /** @description Connected-app name. */
            app_name: string;
            /**
             * Format: int64
             * @description Governed HTTP request body bytes.
             */
            app_request_bytes: number;
            /**
             * Format: int64
             * @description Governed HTTP requests.
             */
            app_requests: number;
            /**
             * Format: int64
             * @description Governed HTTP response body bytes.
             */
            app_response_bytes: number;
            /**
             * Format: int64
             * @description MCP tool calls.
             */
            tool_calls: number;
            /**
             * Format: int64
             * @description MCP request body bytes.
             */
            tool_request_bytes: number;
            /**
             * Format: int64
             * @description MCP response body bytes.
             */
            tool_response_bytes: number;
        };
        /** @description One page of the audit ledger. */
        AuditEventListResponse: {
            /** @description Events, newest activity first. */
            data: components["schemas"]["AuditEventView"][];
            /** @description Pass as `cursor` for the next page, or null at the end of the ledger. */
            next_cursor?: string | null;
        };
        /** @description One recorded administrative action. */
        AuditEventView: {
            /** @description Stable machine-readable action, e.g. `models.provider_created`. */
            action: string;
            /** @description Who acted, as a label. Informational: never an authorization identity. */
            actor: string;
            /**
             * Format: uuid
             * @description Stable event ID.
             */
            id: string;
            /** @description When it last recurred. Equal to `occurred_at` for a single occurrence. */
            last_occurred_at: string;
            /**
             * @description Content-free structured context, `{}` when the event carried none.
             *
             *     Non-secret request metadata only — e.g. the `client_id` and `scope` a
             *     refused device-authorization request presented — never credentials or
             *     request bodies.
             */
            metadata: unknown;
            /** @description When the action was first recorded, RFC 3339 in UTC. */
            occurred_at: string;
            /**
             * Format: int64
             * @description How many occurrences this row represents.
             *
             *     Repeated identical actions collapse into one row rather than flooding
             *     the ledger, so a count above 1 is a real repetition, not a duplicate.
             */
            occurrence_count: number;
            /** @description Whether the action succeeded. */
            outcome: components["schemas"]["OperationOutcome"];
            /**
             * Format: uuid
             * @description The specific object, when the action named one.
             *
             *     Null for an action against a class rather than an instance — a failed
             *     sign-in has no object — and never a substitute for the identity in
             *     `actor`.
             */
            target_id?: string | null;
            /**
             * @description Kind of object the action concerned.
             *
             *     Here so a lifecycle event can be joined to the thing it happened to.
             *     Without it a row saying a policy was disabled names no policy, and the
             *     only way to find out which is to correlate timestamps against the
             *     object listings.
             */
            target_type: components["schemas"]["AuditTargetKind"];
        };
        /**
         * @description Resource category referenced by an administrative audit event.
         * @enum {string}
         */
        AuditTargetKind: "installation" | "session" | "api_token" | "identity_provider" | "user" | "team" | "model_provider" | "provider_model" | "provider_subscription_binding" | "connected_app" | "mcp_endpoint" | "scim_connector" | "sandbox" | "shared_app" | "connector_session" | "cost_limit_policy" | "rate_limit_policy" | "guardrail_policy" | "guardrail_finding" | "guardrail_engine" | "add_on";
        /** @description Account counts behind an authentication-policy decision. */
        AuthenticationPolicyAccounts: {
            /**
             * Format: int64
             * @description Accounts that accept new sessions.
             */
            active: number;
            /**
             * Format: int64
             * @description Active accounts with a password and no enabled verified provider.
             */
            local_only: number;
            /**
             * Format: int64
             * @description Active accounts holding no password at all.
             *
             *     Non-zero means withdrawing the last enabled verified provider is refused
             *     with `sso_only_accounts`.
             */
            passwordless: number;
            /**
             * Format: int64
             * @description Active accounts linked to an enabled verified provider.
             *
             *     Population, not prediction: an account counted here may also hold a
             *     password. See `passwordless` for the cohort a withdrawal is refused over.
             */
            sso: number;
        };
        /** @description The installation's sign-in policy and what it currently applies to. */
        AuthenticationPolicyView: {
            /** @description Who the policy currently applies to. */
            accounts: components["schemas"]["AuthenticationPolicyAccounts"];
            /** @description Which interactive sign-in methods are accepted. */
            login_mode: components["schemas"]["LoginMode"];
            /** @description When the policy was last changed. */
            updated_at: string;
        };
        /**
         * @description Maximum data-retention mode accepted from Bedrock Mantle.
         * @enum {string}
         */
        BedrockDataRetentionPolicy: "none" | "aws_only" | "provider_share";
        /**
         * @description How the capacity that served an inference request is paid for.
         * @enum {string}
         */
        BillingClass: "billed" | "notional" | "credits" | "provisioned";
        /** @description Optional body for `cancelSandbox`. */
        CancelSandboxRequest: {
            /**
             * @description Why the sandbox is being stopped, in a few words. Trimmed; at most 200
             *     characters; recorded as `termination_detail` on the sandbox and in the
             *     `administrator_cancelled` event.
             */
            reason?: string | null;
        };
        /** @description One route whose canonical identity keeps it out of a group it belongs in. */
        CanonicalMismatchMemberView: {
            /** @description Identifier clients request this route by; unchanged by the repair. */
            gateway_id: string;
            /**
             * Format: uuid
             * @description The provider model to repair.
             */
            model_id: string;
            /** @description Owning provider name. */
            provider_name: string;
        };
        /** @description Routes that look like one model but do not group as one. */
        CanonicalMismatchView: {
            /**
             * @description The routes whose canonical identity would change. Routes already
             *     carrying the suggested value are not listed: nothing about them changes.
             */
            members: components["schemas"]["CanonicalMismatchMemberView"][];
            /** @description The canonical identity these routes would share once repaired. */
            suggested_canonical_model_id: string;
        };
        /** @description Traffic grouped by the client that authenticated it. */
        ClientUsageView: {
            /**
             * Format: int64
             * @description Abandoned failover attempts.
             */
            abandoned_requests: number;
            /**
             * Format: int64
             * @description Requests that resolved to a conversation, and so appear in conversation
             *     views. The remainder routed successfully but asserted no conversation
             *     identifier, which is why a working harness can look absent there.
             */
            attributed_requests: number;
            /**
             * Format: int64
             * @description Mean committed-request duration in milliseconds.
             */
            avg_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Mean committed-request time-to-first-byte in milliseconds.
             */
            avg_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Mean northbound message count across committed requests that recorded one.
             */
            avg_message_count?: number | null;
            /**
             * Format: double
             * @description Mean declared tool-definition count across committed requests that recorded one.
             */
            avg_tool_definition_count?: number | null;
            /**
             * Format: double
             * @description Mean user-message count across committed requests that recorded one.
             */
            avg_user_message_count?: number | null;
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Cached (cache-read) input tokens on committed requests.
             */
            cached_input_tokens: number;
            client_name?: null | components["schemas"]["GatewayClient"];
            /**
             * Format: int64
             * @description Committed requests (`attempt_role` is not abandoned).
             */
            committed_requests: number;
            /**
             * Format: int64
             * @description Estimated metered cost, in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Cache-write input tokens on committed first-turn requests.
             */
            first_turn_cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Estimated billed cost of committed first-turn requests, in
             *     micro-US-dollars.
             */
            first_turn_cost_microusd: number;
            /**
             * Format: int64
             * @description Input tokens on committed first-turn requests.
             */
            first_turn_input_tokens: number;
            /**
             * Format: int64
             * @description Committed first-turn requests: one user message and nothing else in
             *     the transcript beyond the standing instructions block — the opening
             *     request of a conversation, never a tool-loop continuation. What
             *     starting a session costs — system prompt, standing instruction files,
             *     and tool definitions land here with nothing yet amortized.
             */
            first_turn_requests: number;
            /**
             * Format: int64
             * @description Requests from this client, including abandoned failovers.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Input tokens across committed requests.
             */
            input_tokens: number;
            /**
             * @description Most recent request, so a silent client is distinguishable from one that
             *     never connected.
             */
            last_activity_at?: string | null;
            /**
             * Format: int64
             * @description Output tokens across committed requests.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Median committed-request duration in milliseconds.
             */
            p50_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Median committed-request time-to-first-byte in milliseconds.
             */
            p50_first_byte_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request duration in milliseconds.
             */
            p95_duration_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request time-to-first-byte in milliseconds.
             */
            p95_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Peak input-tokens / context-window across committed requests.
             */
            peak_context_utilization?: number | null;
            /** Format: int64 */
            pending_rate_requests: number;
            /**
             * Format: int64
             * @description How many of those requests carry a final price.
             */
            priced_requests: number;
            /** Format: int64 */
            provisional_requests: number;
            /**
             * Format: int64
             * @description Committed requests that recorded request-shape counts — the denominator
             *     for the first-turn statistics. History that predates shape capture has
             *     no `user_message_count` and is excluded from them, not counted as zero.
             */
            shape_recorded_requests: number;
            /**
             * Format: int64
             * @description Requests that resolved to no conversation — the same remainder, stated
             *     as its own number so a reader reconciling conversation views against
             *     this rollup does not have to derive it.
             */
            unattributed_requests: number;
            /** Format: int64 */
            unpriceable_requests: number;
        };
        /**
         * @description Cloud workload identity mode shown in provider summaries.
         * @enum {string}
         */
        CloudIdentityMode: "default_chain" | "assume_role" | "application_default" | "service_account_impersonation";
        /**
         * @description Agent-facing interfaces enabled for one governed REST connected app.
         * @enum {string}
         */
        ConnectedAppAccessMode: "proxy_api" | "governed_shell" | "both";
        /** @description Statement list for one app. */
        ConnectedAppAccessPolicyListResponse: {
            data: components["schemas"]["ConnectedAppAccessPolicyView"][];
            /**
             * @description Human-readable choices from the app's declared operation catalog.
             *     Absent only when this app does not expose an enumerable catalog.
             */
            operations?: components["schemas"]["ConnectedAppOperationView"][] | null;
        };
        /** @description One created statement. */
        ConnectedAppAccessPolicyResponse: {
            data: components["schemas"]["ConnectedAppAccessPolicyView"];
        };
        /** @description One stored statement, rendered. */
        ConnectedAppAccessPolicyView: {
            /**
             * Format: uuid
             * @description Constrained app.
             */
            app_id: string;
            /** @description RFC 3339 creation time. */
            created_at: string;
            /**
             * Format: uuid
             * @description Administrator who created the statement; absent for seeded rows.
             */
            created_by?: string | null;
            /** @description `allow` (restrict-to) or `deny`. */
            effect: components["schemas"]["AccessPolicyEffect"];
            /**
             * Format: uuid
             * @description Statement ID.
             */
            id: string;
            /**
             * @description Exact operation identifier, `tool:<name>` on an MCP app, or `*` on a
             *     deny.
             */
            operation: string;
            /**
             * Format: uuid
             * @description Scope instance, absent for app scope.
             */
            scope_id?: string | null;
            /** @description Which principals the statement covers. */
            scope_type: components["schemas"]["AccessPolicyScopeKind"];
        };
        /**
         * @description Population that may reach a connected app through a granted endpoint.
         *
         *     This gates *reachability* only. Every request still carries the calling
         *     user's own authority, so widening the audience cannot widen what any
         *     individual caller is permitted to do.
         * @enum {string}
         */
        ConnectedAppAudience: "administrators" | "all_users";
        /**
         * @description Credential mechanism used by a connected app.
         * @enum {string}
         */
        ConnectedAppAuthMethod: "shared_secret" | "delegated_oauth" | "none" | "gateway_identity";
        /** @description What one app's egress trusts beyond the platform roots. */
        ConnectedAppCaTrustView: {
            /**
             * @description The stored PEM, absent when none is configured.
             *
             *     Returned in full rather than as a presence bit, unlike every credential
             *     on this app: a CA certificate is public material that the upstream hands
             *     to any client that connects to it, so withholding it would protect
             *     nothing and would stop an administrator checking what they pasted.
             */
            ca_trust_bundle?: string | null;
            /** @description Each certificate the bundle asks this app's egress to trust. */
            certificates: components["schemas"]["TrustedCertificateView"][];
            /** @description Whether a bundle is configured at all. */
            configured: boolean;
        };
        /** @description Caller-scoped connected-app connection listing. */
        ConnectedAppConnectionListResponse: {
            /** @description Apps visible on the caller's account surface and their personal status. */
            data: components["schemas"]["ConnectedAppConnectionView"][];
        };
        /**
         * @description One connected app as it relates to the calling account.
         *
         *     These are presence and health facts only. No credential value, OAuth token,
         *     client identifier, scope, or upstream account identity is returned.
         */
        ConnectedAppConnectionView: {
            /** @description Whether the app is enabled and the caller is currently granted it. */
            available: boolean;
            /**
             * Format: uuid
             * @description Stable connected-app ID.
             */
            id: string;
            /** @description Which adapter serves the app. */
            kind: components["schemas"]["ConnectedAppKind"];
            /** @description Administrator-supplied app name. */
            name: string;
            /** @description Whether this app connects the caller through OAuth. */
            oauth: boolean;
            /** @description Whether the caller has an OAuth credential stored for this app. */
            oauth_connected: boolean;
            /** @description UTC expiry reported for the caller's OAuth access token, when known. */
            oauth_expires_at?: string | null;
            /** @description Whether that OAuth credential must be explicitly reauthorized. */
            oauth_reauthorization_required: boolean;
            /** @description Whether the caller has a user-owned static credential stored. */
            personal_credential: boolean;
            /** @description Whether current policy allows the caller to attach a static credential. */
            personal_credentials_allowed: boolean;
        };
        /**
         * @description The credential the gateway will present to the app.
         *
         *     Which members are read depends on `kind` and `auth_method`; the ones that do
         *     not apply must be absent. Every one of them is write-only — no operation
         *     returns stored credential material, and `credential_configured` on the app
         *     view reports only that something is stored.
         */
        ConnectedAppCredentialInput: {
            /**
             * Format: password
             * @description Datadog access token, for `datadog_access_token`.
             */
            access_token?: string | null;
            /**
             * Format: password
             * @description Datadog API key, for `datadog_api_and_application_keys`.
             */
            api_key?: string | null;
            /**
             * Format: password
             * @description Datadog application key, for `datadog_api_and_application_keys`.
             */
            application_key?: string | null;
            /**
             * Format: password
             * @description PEM-encoded RSA private key of the GitHub App a forge mints tokens from.
             *
             *     For `git_forge` apps whose `git_forge.credential_mode` is `github_app`,
             *     and refused everywhere else. Optional even there: the console can create
             *     the app on GitHub on the administrator's behalf, and the key it returns
             *     does not exist until they have clicked through that. A forge configured
             *     without one holds no credential and its egress is denied until it does.
             *
             *     Write-only, like every other member here. No operation in this document
             *     reads a stored key back, and the app view reports only that one exists.
             */
            github_app_private_key?: string | null;
            login?: null | components["schemas"]["GitForgeLogin"];
            /**
             * Format: password
             * @description OAuth client secret, for a `delegated_oauth` app whose issuer needs one.
             */
            oauth_client_secret?: string | null;
            scheme?: null | components["schemas"]["CredentialScheme"];
            /**
             * Format: password
             * @description The single secret a `shared_secret` app presents.
             */
            shared_secret?: string | null;
            /**
             * Format: password
             * @description Login half of a `git_forge` app's HTTP basic credential, for the
             *     `bot_user` login convention only.
             *
             *     Its own field rather than a value guessed at injection time: a bot
             *     account's login is something only the administrator knows. Encrypted
             *     like every other component, which says nothing about it being secret.
             */
            username?: string | null;
        };
        /** @description Effective access for one chosen principal. */
        ConnectedAppEffectiveAccessResponse: {
            data: components["schemas"]["EffectiveOperationView"][];
        };
        /**
         * @description Latest per-app reachability facts (ADR 0069 D3, #1090).
         *
         *     Three independent fact pairs — traffic success, traffic failure, probe —
         *     each answering a different question; readers compare the timestamps to
         *     decide which evidence is current. Policy denials and per-user delegated
         *     grant failures never appear here.
         */
        ConnectedAppHealthView: {
            /** @description When an exchange last failed in a way attributable to the app. */
            last_failure_at?: string | null;
            /** @description `unreachable`, `auth_rejected`, or `upstream_error`. */
            last_failure_code?: string | null;
            /**
             * Format: int32
             * @description HTTP status of that failure, when a response was received at all.
             */
            last_failure_status?: number | null;
            /** @description When an admin probe last ran. */
            last_probe_at?: string | null;
            /**
             * @description Coarse probe classification (`http_<status>`, `dns`, `connect`,
             *     `tls`, `timeout`, `other`); never upstream prose.
             */
            last_probe_detail?: string | null;
            /** @description Whether that probe got any HTTP response. */
            last_probe_ok?: boolean | null;
            /** @description When an upstream exchange last proved the integration alive. */
            last_success_at?: string | null;
            /**
             * Format: int32
             * @description HTTP status of that success.
             */
            last_success_status?: number | null;
        };
        /**
         * @description Supported connected-app adapter.
         * @enum {string}
         */
        ConnectedAppKind: "datadog" | "sentry" | "linear" | "figma" | "slack" | "atlassian" | "pagerduty" | "vercel" | "generic_mcp" | "rest_api" | "model_gateway" | "git_forge" | "gitlab";
        /** @description Connected-app listing. */
        ConnectedAppListResponse: {
            /** @description Every connected app, deleted ones excluded. */
            data: components["schemas"]["ConnectedAppView"][];
        };
        /** @description The delegated-OAuth client configuration a per-user grant is obtained with. */
        ConnectedAppOauthInput: {
            /** @description Authorization endpoint the user is sent to. */
            authorization_url?: string | null;
            /** @description OAuth client identifier registered with the app. */
            client_id?: string | null;
            /**
             * @description Discover the upstream's authorization server and register a client
             *     dynamically at create time (ADR 0069 D4). When set, the three manual
             *     OAuth fields are left empty and filled by discovery; a server without
             *     dynamic registration is refused with `oauth_registration_unsupported`
             *     naming the manual path as the remedy.
             */
            discover?: boolean;
            /** @description Space-separated scopes requested for each user's grant. */
            scopes?: string | null;
            /** @description Token endpoint the authorization code is exchanged at. */
            token_url?: string | null;
        };
        /** @description One operation an administrator can choose without knowing its internal key. */
        ConnectedAppOperationView: {
            /** @description HTTP method for governed REST operations, when meaningful. */
            method?: string | null;
            /** @description Stable catalog key sent back when the operation is selected. */
            operation_id: string;
            /** @description Path template for governed REST operations, when meaningful. */
            path?: string | null;
            /** @description Plain-language description supplied by the catalog, when present. */
            summary?: string | null;
        };
        /**
         * @description A connected app and the terms it is reachable on.
         *
         *     The bools are independent facts about one app — enabled, built in, and two
         *     separate credential-presence bits — not a state machine. Each is read by a
         *     different decision, and collapsing them into an enum would invent
         *     combinations that do not exist while hiding the ones that do.
         */
        ConnectedAppView: {
            agent_access_mode?: null | components["schemas"]["ConnectedAppAccessMode"];
            /** @description The allowed routes, empty when the mode is `all`. */
            allowed_model_ids: string[];
            /** @description Who may reach the app. Meaningful only for the built-in app. */
            audience: components["schemas"]["ConnectedAppAudience"];
            /** @description How the gateway authenticates to it. */
            auth_method: components["schemas"]["ConnectedAppAuthMethod"];
            /** @description Whether the gateway provisions this app itself. */
            built_in: boolean;
            /** @description Whether a credential is actually present. */
            credential_configured: boolean;
            credential_scheme?: null | components["schemas"]["CredentialScheme"];
            /** @description Whether the app may be invoked at all. */
            enabled: boolean;
            /** @description Origin the gateway calls. */
            endpoint_url: string;
            git_forge_credential_mode?: null | components["schemas"]["GitForgeCredentialMode"];
            /**
             * Format: int64
             * @description GitHub App a `github_app` forge mints from, once it has one.
             */
            git_forge_github_app_id?: number | null;
            /**
             * Format: int64
             * @description Installation the forge mints for, once a human has approved one.
             */
            git_forge_github_app_installation_id?: number | null;
            /**
             * @description When this gateway process last minted a token for the configured GitHub
             *     App installation, if it has.
             *
             *     Per-process: minted tokens live in memory and are never stored, so a
             *     replica that has not minted since it started reports nothing. Null is
             *     "no evidence here", never "broken", and is also the ordinary value for
             *     apps that are not GitHub App-backed git forges.
             */
            git_forge_github_app_last_minted_at?: string | null;
            /** @description Slug that app's pages are addressed by, once it is known. */
            git_forge_github_app_slug?: string | null;
            /**
             * @description Whether this `git_forge` app carries the ADR 0052 intrinsic GitHub REST
             *     channel (`github_app` mode and a stored OAuth client).
             *
             *     Distinct from `rest_tool_slug` / `rest_operation_count`, which the SPA
             *     treats as "this is a REST app". The catalog ships with the kind; it is
             *     not a `rest_api_connections` row.
             */
            git_forge_has_intrinsic_rest_channel: boolean;
            /**
             * Format: int64
             * @description How many operations that channel exposes, when it is active.
             */
            git_forge_intrinsic_rest_operation_count?: number | null;
            health?: null | components["schemas"]["ConnectedAppHealthView"];
            /**
             * Format: uuid
             * @description Stable app ID.
             */
            id: string;
            /** @description Which adapter serves the app. */
            kind: components["schemas"]["ConnectedAppKind"];
            /** @description Which model routes may invoke the app. */
            model_access_mode: components["schemas"]["ModelAccessMode"];
            /** @description Administrator-supplied name. */
            name: string;
            /**
             * @description Whether the app row holds an OAuth client the gateway can start a
             *     per-user authorization with.
             *
             *     Not the installation credential `credential_configured` reports: that is
             *     how the gateway authenticates itself, this is whether an individual user
             *     can be sent through Connect to authorize on their own behalf. The two
             *     are independent — a `github_app` forge has the first and, unless its
             *     GitHub App conversion carried an OAuth client, not the second. That
             *     conversion is the only writer and runs once, so a forge that completed
             *     it without a client cannot grow one and has to be recreated. That is the
             *     whole triage question, and it was previously unreadable.
             *
             *     A presence bit only. Neither the client id nor its secret is returned by
             *     this or any other read.
             */
            oauth_client_configured: boolean;
            oauth_client_registration?: null | components["schemas"]["OauthClientRegistration"];
            /**
             * @description When the DCR-issued client secret expires, RFC 3339. Surfaced, never
             *     auto-rotated; null when no expiry is known.
             */
            oauth_client_secret_expires_at?: string | null;
            /**
             * @description Issuer a dynamically registered OAuth client belongs to (ADR 0069
             *     D5); null for manually registered apps.
             */
            oauth_issuer?: string | null;
            /**
             * @description Space-separated scopes sent on future authorization redirects. This is
             *     non-secret configuration; existing grants keep their recorded scopes.
             */
            oauth_scopes?: string | null;
            /** @description Whether a caller's own credential may stand in for the installation's. */
            personal_credential_policy: components["schemas"]["PersonalCredentialPolicy"];
            /**
             * Format: int64
             * @description How many operations that projection exposes.
             */
            rest_operation_count?: number | null;
            /**
             * @description MCP tool slug of the governed REST projection, when there is one.
             *
             *     Non-null means this *is* a REST app. A `git_forge` app's intrinsic
             *     GitHub REST channel does not populate this — see
             *     `git_forge_has_intrinsic_rest_channel`.
             */
            rest_tool_slug?: string | null;
        };
        /** @description An access profile. */
        ConnectorAccessProfileView: {
            /**
             * Format: uuid
             * @description Connection.
             */
            connection_id: string;
            /**
             * Format: uuid
             * @description Profile id.
             */
            id: string;
            /** @description Slug. */
            slug: string;
        };
        /** @description A logical connection. */
        ConnectorConnectionView: {
            /** @description Database slug. */
            database_slug: string;
            /**
             * Format: uuid
             * @description Connection id.
             */
            id: string;
            /**
             * Format: uuid
             * @description Site.
             */
            site_id: string;
            /** @description Slug. */
            slug: string;
        };
        /** @description A grant. */
        ConnectorGrantView: {
            /**
             * Format: uuid
             * @description Connection.
             */
            connection_id: string;
            /**
             * Format: uuid
             * @description Grant id.
             */
            id: string;
            /**
             * Format: uuid
             * @description Profile.
             */
            profile_id: string;
            /**
             * Format: uuid
             * @description Grantee.
             */
            user_id: string;
        };
        /** @description A connector instance. */
        ConnectorInstanceView: {
            /** @description Disabled. */
            disabled: boolean;
            /**
             * Format: uuid
             * @description Connector id.
             */
            id: string;
            /** @description Name. */
            name: string;
            /**
             * Format: uuid
             * @description Site.
             */
            site_id: string;
        };
        /** @description Site listing. */
        ConnectorSiteListResponse: {
            /** @description Sites. */
            data: components["schemas"]["ConnectorSiteView"][];
        };
        /** @description A site. */
        ConnectorSiteView: {
            /**
             * Format: uuid
             * @description Site id.
             */
            id: string;
            /** @description Name. */
            name: string;
            /** @description Slug. */
            slug: string;
        };
        /** @description One inference request within a conversation, with cost-debugging fields. */
        ConversationCostEventView: {
            /** @description `primary`, `failover`, or `abandoned`. Null means unrecorded. */
            attempt_role?: string | null;
            /** @description Platform task kind when this is background model work. */
            background_task_kind?: string | null;
            /**
             * @description Client-supplied item identifier within the Message Batch.
             * @example job-001
             */
            batch_custom_id?: string | null;
            /**
             * @description Gateway-owned Message Batch identifier for an asynchronous item.
             * @example msgbatch_01k39f4fb5x7x0q2a1m6s9v8te
             */
            batch_id?: string | null;
            /** @description How the serving capacity was paid for. */
            billing_class: components["schemas"]["BillingClass"];
            /**
             * Format: int64
             * @description One-hour cache-write rate snapshot.
             */
            cache_write_1h_price_microusd_per_million?: number | null;
            /**
             * Format: int64
             * @description Five-minute cache-write rate snapshot.
             */
            cache_write_5m_price_microusd_per_million?: number | null;
            /**
             * Format: int64
             * @description Known cache-write input tokens.
             */
            cache_write_input_tokens?: number | null;
            /**
             * Format: int64
             * @description Cache-read rate snapshot; null means the input rate applied.
             */
            cached_input_price_microusd_per_million?: number | null;
            /**
             * Format: int64
             * @description Known cached (cache-read) input tokens.
             */
            cached_input_tokens?: number | null;
            /**
             * Format: int32
             * @description Catalog context window of the serving route.
             */
            context_window?: number | null;
            /**
             * @description `priced`, `pending_rate`, `provisional`, or `unpriceable`.
             * @example pending_rate
             */
            cost_state: string;
            /**
             * @description Credential class that produced this event: `user_subscription`,
             *     `provider_api_key`, `workload_identity`, or `none`.
             * @example user_subscription
             */
            credential_class: string;
            /**
             * @description Why resolution chose this credential (ADR 0027). Null when the provider
             *     has no subscription dimension or the row predates the vocabulary.
             * @example subscription_used
             */
            credential_resolution_reason?: string | null;
            /**
             * Format: int64
             * @description End-to-end latency in milliseconds.
             */
            duration_ms: number;
            /**
             * Format: int64
             * @description Estimated request cost in micro-US-dollars.
             */
            estimated_cost_microusd?: number | null;
            /**
             * @description True when a subscription was attempted first and a metered credential
             *     served instead.
             */
            fallback_from_subscription: boolean;
            /**
             * @description The provider's own finish or stop reason, raw and unnormalized. Null
             *     means the provider reported none.
             * @example tool_calls
             */
            finish_reason?: string | null;
            /**
             * Format: int64
             * @description Time-to-first-byte in milliseconds, when an upstream response was observed.
             */
            first_byte_ms?: number | null;
            /** @description Client-visible model ID, as recorded on the event itself. */
            gateway_model_id: string;
            /**
             * Format: uuid
             * @description Usage event ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Input rate snapshot used by this event's estimate.
             */
            input_price_microusd_per_million?: number | null;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens?: number | null;
            /**
             * Format: int32
             * @description Northbound turn items presented to the model.
             */
            message_count?: number | null;
            /**
             * Format: uuid
             * @description Catalog model, or null when the event predates model attribution or its
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /** @description UTC event timestamp, RFC3339. */
            occurred_at: string;
            /**
             * Format: int64
             * @description Output rate snapshot used by this event's estimate.
             */
            output_price_microusd_per_million?: number | null;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens?: number | null;
            /** @description Provider display name, as recorded on the event itself. */
            provider_name: string;
            provisioned_capacity_evidence?: null | components["schemas"]["ProvisionedCapacityEvidence"];
            /**
             * Format: int64
             * @description Known reasoning output tokens.
             */
            reasoning_output_tokens?: number | null;
            /**
             * Format: uuid
             * @description The client-visible `x-request-id` this event was recorded under, when
             *     one was captured. `getInferenceRequest` traces the same ID back to
             *     this event.
             */
            request_id?: string | null;
            /** @description `user_request`, `background_model_task`, or `message_batch`. */
            source_kind: string;
            /**
             * Format: uuid
             * @description The subscription binding this event drew on, or fell back from when
             *     `fallback_from_subscription` is true. Null when none was involved.
             */
            subscription_binding_id?: string | null;
            /**
             * @description That binding's label, as shown on the account page. Null when the
             *     binding has since been deleted or none was involved.
             */
            subscription_label?: string | null;
            /**
             * @description Email of the account that owns the binding; a teammate's under team
             *     sharing (ADR 0037).
             */
            subscription_owner_email?: string | null;
            /**
             * @description How the upstream response ended: `completed`, `incomplete`, `truncated`,
             *     or `failed`. Null means unrecorded — the event predates this vocabulary
             *     or the path that served it reports no terminal state — and must not be
             *     read as a completed response.
             * @example truncated
             */
            terminal_state?: string | null;
            /**
             * Format: int32
             * @description Tool or function call items the response carried. Null means uncounted,
             *     which is distinct from a counted zero.
             */
            tool_call_count?: number | null;
            /**
             * Format: int32
             * @description Declared tool/function definitions on the request.
             */
            tool_definition_count?: number | null;
            /**
             * Format: int32
             * @description Northbound items with role=user.
             */
            user_message_count?: number | null;
        };
        /**
         * @description Client family associated with an attributed model conversation.
         * @enum {string}
         */
        ConversationHarness: "claude_code" | "claude_desktop" | "codex" | "pi" | "opencode" | "omp" | "grok_build" | "tidebreak" | "gateway_client" | "custom";
        /** @description One page of the conversation listing. */
        ConversationListResponse: {
            /**
             * @description Conversations in the requested order; most recently active first by
             *     default.
             */
            data: components["schemas"]["ConversationSummaryView"][];
            /**
             * @description Pass as `cursor` for the next page, or null at the end of the listing.
             *
             *     Always null when `ranked` is true: a ranking has no page after it.
             */
            next_cursor?: string | null;
            /**
             * @description Whether `data` is a capped ranking rather than a page.
             *
             *     True when `sort` names a window aggregate (`spend`, `subscription`,
             *     `provisioned`, `tokens`, `requests`). Those keys are sums over the events
             *     inside the selected window rather than stored columns, so they are
             *     answered as the top `limit` and cannot be paged. Widen the window or
             *     raise `limit` to see more; do not expect a cursor.
             */
            ranked: boolean;
        };
        /**
         * @description One project a conversation has been observed working in (ADR 0059).
         *
         *     Membership only: a host-stripped `owner/repo` reduced from
         *     the harness process's git remote, never a path, a prompt, or a transcript.
         *     The set carries no cost columns by design — spend is keyed on the slug
         *     stamped on each inference event, not on the conversation's membership.
         */
        ConversationProjectView: {
            /** @description UTC first observation, RFC3339. */
            first_seen_at: string;
            /** @description UTC most recent observation, RFC3339. */
            last_seen_at: string;
            /**
             * @description Latest branch or short SHA seen for this slug. Branch churn does not
             *     make a new project, so this moves while the row stays.
             */
            repo_ref?: string | null;
            /**
             * @description Host-stripped `owner/repo`.
             * @example acme/checkout
             */
            repo_slug: string;
            /**
             * @description Whether events may be assigned this slug: `allocatable` for the one slug
             *     that may receive event dollars, `environment` for a project that is
             *     visible on the conversation and is never a spend key.
             * @example allocatable
             */
            role: string;
            /**
             * @description How the row was last observed: `modelctl_cwd`, `broker_cwd`, or
             *     `sandbox_declared`.
             * @example modelctl_cwd
             */
            source: string;
            /** @description Directory basename, when one was observed. */
            workspace_name?: string | null;
        };
        /**
         * @description One conversation as a listing row.
         *
         *     The aggregates the detail read reports, minus its per-request latency
         *     distribution and its event list: what a caller needs to pick the
         *     conversation worth pulling with `getConversation`, and no more.
         */
        ConversationSummaryView: {
            /**
             * Format: int64
             * @description Known child/teammate agents.
             */
            agent_count: number;
            /**
             * Format: int64
             * @description Governed connected-app HTTP requests.
             */
            app_requests: number;
            /**
             * @description `exact` for a server-observed harness identifier, `client_asserted` for
             *     a generic one the client supplied. Traffic grouped under a
             *     `client_asserted` identity is only as trustworthy as the client.
             * @example exact
             */
            attribution_quality: string;
            /**
             * @description Where the conversation identity came from, e.g. `claude_code_native`.
             * @example claude_code_native
             */
            attribution_source: string;
            /**
             * Format: int64
             * @description Known input tokens served from a provider cache.
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Cost drawn from prepaid usage credits, in micro-US-dollars.
             */
            credits_cost_microusd: number;
            /** @description Whether the conversation is non-resumable. */
            ephemeral: boolean;
            /**
             * Format: int64
             * @description Estimated metered cost in micro-US-dollars. A floor rather than a total
             *     wherever `priced_request_count` is below `inference_requests`.
             */
            estimated_cost_microusd: number;
            /** @description UTC first-observed timestamp, RFC3339. */
            first_seen_at: string;
            /**
             * @description Normalized harness kind.
             * @example claude_code
             */
            harness: string;
            /**
             * Format: uuid
             * @description Gateway conversation ID. Pass to `getConversation`.
             */
            id: string;
            /**
             * Format: int64
             * @description Committed model request count.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens: number;
            /** @description UTC most-recent activity timestamp, RFC3339. The sort key. */
            last_activity_at: string;
            /**
             * Format: int64
             * @description Prepaid-credit cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_credits_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Metered cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_estimated_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Provisioned-capacity cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_provisioned_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Notional subscription cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_subscription_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Distinct gateway models used.
             */
            model_count: number;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Requests awaiting one or more rate dimensions.
             */
            pending_rate_request_count: number;
            /**
             * Format: int64
             * @description Requests carrying price estimates.
             */
            priced_request_count: number;
            /**
             * Format: int32
             * @description Distinct slugs this conversation has been seen with; above one means
             *     `repo_slug` is the most recent of several.
             */
            project_slug_count: number;
            /**
             * Format: int64
             * @description Distinct provider instances used.
             */
            provider_count: number;
            /**
             * Format: int64
             * @description Requests with complete rates but approximate provider usage.
             */
            provisional_request_count: number;
            /**
             * Format: int64
             * @description Cost absorbed by provider-provisioned capacity, in micro-US-dollars.
             */
            provisioned_cost_microusd: number;
            /** @description Latest observed branch or short SHA. */
            repo_ref?: string | null;
            /**
             * @description Latest observed host-stripped `owner/repo` (ADR 0059).
             *
             *     A display handle, not a spend key: cost is booked on the slug stamped on
             *     each inference event, so a conversation resumed in another clone reports
             *     the newest slug over events that kept the old one. Summing these rows by
             *     `repo_slug` misattributes spend; `getConversation` lists the full set.
             * @example acme/checkout
             */
            repo_slug?: string | null;
            /**
             * Format: int64
             * @description Exact client runs, when the client supplied them.
             */
            run_count: number;
            /**
             * Format: int64
             * @description Cost a user's own subscription absorbed, in micro-US-dollars. Notional:
             *     the installation was not billed for it.
             */
            subscription_cost_microusd: number;
            /**
             * Format: int64
             * @description Requests served on a user subscription credential.
             */
            subscription_request_count: number;
            /** @description Generated conversation title, or null until enough user context exists. */
            title?: string | null;
            /**
             * Format: int64
             * @description Proven MCP/tool calls.
             */
            tool_calls: number;
            /**
             * Format: int64
             * @description Requests with no usable provider token counts.
             */
            unpriceable_request_count: number;
            /** @description Display-ready user name. */
            user_display_name: string;
            /** @description Display-ready user email. */
            user_email: string;
            /**
             * Format: uuid
             * @description Owning user.
             */
            user_id: string;
            /** @description Latest observed workspace directory basename. */
            workspace_name?: string | null;
        };
        /** @description Administrator-selected logical model for automatic conversation titles. */
        ConversationTitleTaskView: {
            /** @description Administrator-facing model name. */
            display_name?: string | null;
            /** @description Whether new title tasks are accepted and processed. */
            enabled: boolean;
            /** @description Client-visible gateway ID of the selected logical model. */
            gateway_id?: string | null;
            /**
             * Format: uuid
             * @description Primary gateway-model catalog row, or null when unconfigured.
             */
            model_id?: string | null;
        };
        /** @description One conversation and its inference events. */
        ConversationView: {
            /**
             * Format: int64
             * @description Mean committed-request duration in milliseconds.
             */
            avg_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Mean committed-request time-to-first-byte in milliseconds.
             */
            avg_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Mean user-message count across committed requests that recorded one.
             */
            avg_user_message_count?: number | null;
            /**
             * Format: int64
             * @description Known input tokens served from a provider cache.
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Cost drawn from prepaid usage credits, in micro-US-dollars.
             */
            credits_cost_microusd: number;
            /** @description Whether the conversation is non-resumable. */
            ephemeral: boolean;
            /**
             * Format: int64
             * @description Estimated metered cost in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /** @description Inference events, newest first, with cost-debugging fields. At most 500. */
            events: components["schemas"]["ConversationCostEventView"][];
            /**
             * @description Whether the conversation holds more events than `events` carries. When
             *     true, `events` is the 500 most recent and the earlier ones are not in
             *     this response; `inference_requests` still counts the whole conversation.
             */
            events_truncated: boolean;
            /** @description UTC first-observed timestamp, RFC3339. */
            first_seen_at: string;
            /**
             * @description Normalized harness kind.
             * @example claude_code
             */
            harness: string;
            /**
             * Format: uuid
             * @description Gateway conversation ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Model request count.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens: number;
            /** @description UTC most-recent activity timestamp, RFC3339. */
            last_activity_at: string;
            /**
             * Format: int64
             * @description Prepaid-credit cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_credits_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Metered cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_estimated_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Provisioned-capacity cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_provisioned_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Subscription cost added by the latest user turn, in micro-US-dollars.
             */
            last_turn_subscription_cost_microusd?: number | null;
            /**
             * Format: int64
             * @description Distinct gateway models used.
             */
            model_count: number;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Median committed-request duration in milliseconds.
             */
            p50_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Median committed-request time-to-first-byte in milliseconds.
             */
            p50_first_byte_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request duration in milliseconds.
             */
            p95_duration_ms?: number | null;
            /**
             * Format: double
             * @description Peak input-tokens / context-window across committed requests.
             */
            peak_context_utilization?: number | null;
            /**
             * Format: int64
             * @description Requests awaiting one or more rate dimensions.
             */
            pending_rate_request_count: number;
            /**
             * Format: int64
             * @description Requests carrying price estimates.
             */
            priced_request_count: number;
            /**
             * Format: int32
             * @description How many distinct slugs this conversation has been seen with. Above one
             *     means `repo_slug` names only the most recent of them.
             */
            project_slug_count: number;
            /**
             * @description Every project this conversation has been observed in (ADR 0059),
             *     most recently seen first. Empty when nothing was ever observed, which
             *     is the ordinary case for a client that posts no project snapshot.
             */
            projects: components["schemas"]["ConversationProjectView"][];
            /**
             * Format: int64
             * @description Distinct provider instances used.
             */
            provider_count: number;
            /**
             * Format: int64
             * @description Requests with complete rates but approximate provider usage.
             */
            provisional_request_count: number;
            /**
             * Format: int64
             * @description Cost provider-provisioned capacity absorbed, in micro-US-dollars.
             */
            provisioned_cost_microusd: number;
            /** @description Latest observed branch or short SHA. Likewise a display handle. */
            repo_ref?: string | null;
            /**
             * @description Latest observed host-stripped `owner/repo` (ADR 0059).
             *
             *     A display handle and nothing more: a conversation that resumed in
             *     another clone reports the last one it was seen in, while the events it
             *     recorded earlier keep the slug they were stamped with. Grouping this
             *     conversation's whole cost under this slug is exactly the error ADR 0059
             *     rejects; `projects` lists the full set, and spend by repo is a `listUsage`
             *     question.
             * @example acme/checkout
             */
            repo_slug?: string | null;
            route_pin?: null | components["schemas"]["RouteAffinityPinView"];
            /**
             * Format: int64
             * @description Cost a user's own subscription absorbed, in micro-US-dollars.
             */
            subscription_cost_microusd: number;
            /** @description Generated conversation title, or null until enough user context exists. */
            title?: string | null;
            /**
             * Format: int64
             * @description Requests with no usable provider token counts.
             */
            unpriceable_request_count: number;
            /** @description Display-ready user name. */
            user_display_name: string;
            /** @description Display-ready user email. */
            user_email: string;
            /**
             * Format: uuid
             * @description Owning user.
             */
            user_id: string;
            /** @description Latest observed workspace directory basename. */
            workspace_name?: string | null;
        };
        CostControlSettingsView: {
            time_zone: string;
            updated_at: string;
        };
        /** @description Advisory findings over the window, ranked. */
        CostFindingsResponse: {
            /** @description Findings, highest accounted spend first. */
            findings: components["schemas"]["CostFindingView"][];
            /** @description Inclusive RFC3339 lower bound actually applied, including the default. */
            since?: string | null;
            /**
             * @description Subjects the window held. No findings over a non-zero subject count is
             *     a clean bill of health; zero subjects means there was nothing to read.
             */
            subjects_examined: number;
            /** @description Exclusive RFC3339 upper bound actually applied. */
            until?: string | null;
        };
        /** @description One advisory cost finding (ADR 0061). */
        CostFindingView: {
            /**
             * Format: double
             * @description What it was compared against, in the same unit.
             */
            baseline: number;
            /**
             * @description Authenticating client the finding is about, or null for traffic that
             *     named none.
             * @example claude-code
             */
            client_name?: string | null;
            /**
             * Format: int64
             * @description Committed attempts behind the finding; abandoned failover attempts are
             *     excluded.
             */
            committed_requests: number;
            /**
             * Format: int64
             * @description Committed conversation-opening attempts in the window.
             */
            first_turn_requests: number;
            /**
             * @description Client-visible model ID, as recorded on the events rather than read
             *     from the live catalog, which may have since renamed the row.
             * @example claude-opus-5
             */
            gateway_id?: string | null;
            /**
             * Format: int64
             * @description Spend over the window the finding accounts for, in micro-USD. Null
             *     where the saving depends on choosing a different route, which the
             *     gateway does not price.
             */
            impact_microusd?: number | null;
            /**
             * @description Which rule fired: `oversized_fixed_context`,
             *     `cache_writes_exceed_savings`, or `low_context_utilization`.
             * @example oversized_fixed_context
             */
            kind: string;
            /**
             * Format: uuid
             * @description Catalog model, or null when the events predate model attribution or the
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /**
             * Format: double
             * @description What this subject does, in the rule's unit: preamble bytes, micro-USD
             *     of cache-write premium, or a share of the context window between 0
             *     and 1.
             */
            observed: number;
            /** @description Provider display name, likewise recorded on the events. */
            provider_name?: string | null;
            /**
             * Format: int64
             * @description Committed attempts that recorded a request shape at all — the
             *     denominator for every shape-derived number here (ADR 0031).
             */
            shape_recorded_requests: number;
        };
        CostLimitListResponse: {
            data: components["schemas"]["CostLimitView"][];
        };
        CostLimitResponse: {
            data: components["schemas"]["CostLimitView"];
        };
        /** @enum {string} */
        CostLimitScopeType: "installation" | "user" | "team" | "gateway_model" | "connected_app";
        CostLimitView: {
            applies_to: string;
            created_at: string;
            /** Format: int64 */
            current_spend_microusd: number;
            enabled: boolean;
            exceeded: boolean;
            /** Format: uuid */
            id: string;
            /** Format: int64 */
            limit_microusd: number;
            resets_at: string;
            /** Format: uuid */
            scope_id?: string | null;
            scope_label: string;
            scope_type: string;
            source: string;
            updated_at: string;
            /** Format: double */
            used_fraction: number;
            warning_reached: boolean;
            window: string;
            window_start: string;
        };
        /**
         * @description What a multiplier write did.
         * @enum {string}
         */
        CostMultiplierOutcome: "applied" | "unchanged";
        /** @description Every multiplier in force for one model. */
        CostMultipliersResponse: {
            data: components["schemas"]["CostMultiplierView"][];
        };
        /** @description Result of setting a multiplier. */
        CostMultiplierUpdateView: {
            outcome: components["schemas"]["CostMultiplierOutcome"];
            /** @description Whether events waiting on this multiplier were queued for repricing. */
            reconciliation_started: boolean;
        };
        /** @description One multiplier in force for a model. */
        CostMultiplierView: {
            /**
             * @description Pinned geography for a residency multiplier, `*` for any pinned geography, or null
             *     for a batch or negotiated discount, which is not keyed by geography.
             */
            geo?: string | null;
            /** @description What the multiplier represents. */
            kind: string;
            /** @description The factor as a decimal string, e.g. `"1.1"`. */
            multiplier: string;
            /**
             * @description Whether an administrator entered this value or the gateway seeded a published one.
             *
             *     A seeded value is a real, applied multiplier rather than a suggestion, and replacing
             *     it is an override rather than a first entry. The console says which it is so nobody
             *     mistakes an inherited number for a decision the installation made.
             */
            source: string;
        };
        /** @description Event count for one `cost_state`. */
        CostStateCountView: {
            /**
             * @description `priced`, `pending_rate`, `provisional`, or `unpriceable`.
             * @example pending_rate
             */
            cost_state: string;
            /**
             * Format: int64
             * @description Number of inference events carrying that classification.
             */
            event_count: number;
        };
        /** @description Cost-state distribution, overall and per model. */
        CostStateSummaryResponse: {
            /**
             * @description The same counts broken out by model, so unpriced traffic can be
             *     attributed without a second call per model.
             */
            by_model: components["schemas"]["ModelCostStateCountView"][];
            /**
             * @description The raw upstream finish reasons behind those states, per model. Rows
             *     are ordered by event count descending and capped; see
             *     `finish_reasons_truncated`.
             */
            finish_reasons: components["schemas"]["ModelFinishReasonCountView"][];
            /**
             * @description True when the window held more distinct finish-reason groups than the
             *     response carries. The reason is provider-controlled text, so the row
             *     count has no bound a schema can promise; what the cap drops is the
             *     lowest-volume tail. Narrow the window or model filter to see it.
             */
            finish_reasons_truncated: boolean;
            /** @description Counts across the matched window, one row per `cost_state` observed. */
            overall: components["schemas"]["CostStateCountView"][];
            /**
             * @description How responses ended, per model. Unlike the `cost_state` counts these do
             *     not partition the window: `terminal_state` is nullable and is never
             *     backfilled, so the null group is reported as its own row and any
             *     aggregate over these counts is over recorded events only.
             */
            terminal_states: components["schemas"]["ModelTerminalStateCountView"][];
            /** @description Permanently unpriceable traffic grouped by its content-free failure shape. */
            unpriceable_causes: components["schemas"]["UnpriceableCauseView"][];
        };
        /** @description Statement to create. */
        CreateConnectedAppAccessPolicyRequest: {
            /** @description `deny` subtracts and is never overridden; `allow` is a restrict-to set. */
            effect: components["schemas"]["AccessPolicyEffect"];
            /**
             * @description Exact operation identifier from the app's ceiling, `tool:<name>` for
             *     one tool on an MCP app, or `*` on a deny.
             */
            operation: string;
            /**
             * Format: uuid
             * @description Team, user, or sandbox-profile ID; absent exactly for app scope.
             */
            scope_id?: string | null;
            /** @description Which principals the statement covers. */
            scope_type: components["schemas"]["AccessPolicyScopeKind"];
        };
        /** @description Request to create a connected app. */
        CreateConnectedAppRequest: {
            auth_method?: null | components["schemas"]["ConnectedAppAuthMethod"];
            credential?: null | components["schemas"]["ConnectedAppCredentialInput"];
            /**
             * @description Origin the gateway calls. Defaults to `https://sentry.io` for `sentry`,
             *     Linear's hosted MCP endpoint for `linear`, and `https://api.figma.com`
             *     for `figma`, which accepts nothing else. Slack's hosted MCP endpoint for
             *     `slack`, Atlassian's hosted Rovo MCP endpoint for `atlassian`,
             *     `https://api.pagerduty.com` for `pagerduty`, which also accepts
             *     `https://api.eu.pagerduty.com` and nothing else, `https://api.vercel.com`
             *     for `vercel`, which accepts nothing else, and `https://gitlab.com` for
             *     `gitlab`. A self-managed GitLab names its instance here, path prefix
             *     included; every OAuth endpoint is derived from it. Where an origin is
             *     pinned, any other value is refused rather than stored.
             */
            endpoint_url?: string | null;
            git_forge?: null | components["schemas"]["GitForgeConnectionInput"];
            /** @description Which adapter serves the app. */
            kind: components["schemas"]["ConnectedAppKind"];
            /** @description Administrator-facing name, unique across apps. */
            name: string;
            oauth?: null | components["schemas"]["ConnectedAppOauthInput"];
            rest_api?: null | components["schemas"]["RestApiConnectionInput"];
        };
        /** @description Create-profile body. */
        CreateConnectorAccessProfileRequest: {
            /**
             * Format: uuid
             * @description Connection.
             */
            connection_id: string;
            /** @description `read` or `write`. */
            slug: string;
        };
        /** @description Create-connection body. */
        CreateConnectorConnectionRequest: {
            /** @description Database slug. */
            database_slug: string;
            /**
             * Format: uuid
             * @description Site.
             */
            site_id: string;
            /** @description Logical slug. */
            slug: string;
        };
        /** @description Create-grant body. */
        CreateConnectorGrantRequest: {
            /**
             * Format: uuid
             * @description Connection.
             */
            connection_id: string;
            /**
             * Format: uuid
             * @description Profile.
             */
            profile_id: string;
            /**
             * Format: uuid
             * @description Grantee.
             */
            user_id: string;
        };
        /** @description Create-connector body. */
        CreateConnectorInstanceRequest: {
            /** @description Name. */
            name: string;
            /**
             * Format: uuid
             * @description Site.
             */
            site_id: string;
        };
        /** @description Create-site body. */
        CreateConnectorSiteRequest: {
            /** @description Name. */
            name: string;
            /** @description Slug. */
            slug: string;
        };
        /**
         * @description One cap to create. Scope, traffic split, and window fix which requests the
         *     cap sees and cannot be changed afterwards; the amount and the switch can.
         */
        CreateCostLimitRequest: {
            /**
             * @description Which traffic the cap sees: `all` for every metered request in scope,
             *     `sandbox` for gateway-hosted sandbox spend only. Defaults to `all`.
             */
            applies_to?: components["schemas"]["AppliesTo"];
            /**
             * @description Whether the cap is enforced. Defaults to true; a disabled policy is
             *     retained and tracked but never denies a request.
             */
            enabled?: boolean;
            /**
             * Format: int64
             * @description The cap, in micro-US-dollars: 1 000 000 to the dollar. `0` denies the
             *     scope's metered inference outright rather than meaning "unlimited".
             */
            limit_microusd: number;
            /**
             * Format: uuid
             * @description The account, team, or gateway model this cap applies to — from
             *     `listPeople`, `listTeams`, or `listProviderModels` respectively. Null
             *     for `installation`, required for every other scope.
             */
            scope_id?: string | null;
            /**
             * @description What the cap covers. `installation` is the whole gateway; every other
             *     value names a target in `scope_id`. `connected_app` is reserved and
             *     refused until inference spend can be attributed to an app.
             */
            scope_type: components["schemas"]["CostLimitScopeType"];
            /**
             * @description The calendar window spend is accumulated over, in the installation's
             *     configured time zone. One policy per scope, traffic split, and window.
             */
            window: components["schemas"]["WindowKind"];
        };
        /** @description Newly created connector, including the one-time bootstrap token. */
        CreatedConnectorInstance: {
            /** @description Bootstrap token. Shown once. */
            bootstrap_token: string;
            /** @description Connector. */
            connector: components["schemas"]["ConnectorInstanceView"];
        };
        /** @description A newly created endpoint. */
        CreatedMcpEndpoint: {
            /**
             * Format: uuid
             * @description The new endpoint's ID.
             */
            id: string;
        };
        /** @description A newly created egress bundle. */
        CreatedSandboxEgressBundle: {
            /**
             * Format: uuid
             * @description The new bundle's ID.
             */
            id: string;
        };
        /** @description A newly created profile. */
        CreatedSandboxProfile: {
            /**
             * Format: uuid
             * @description The new profile's ID.
             */
            id: string;
        };
        /** @description A newly created runner class. */
        CreatedSandboxRunnerClass: {
            /**
             * Format: uuid
             * @description The new class's ID.
             */
            id: string;
        };
        /** @description A newly created SCIM connector, with the token this reply alone carries. */
        CreatedScimConnectorView: {
            /**
             * Format: uuid
             * @description Stable connector ID.
             */
            id: string;
            /** @description Administrator-facing name. */
            name: string;
            /** @description The base URL to configure the directory with. */
            scim_base_url: string;
            /**
             * Format: password
             * @description The bearer token. Returned here and nowhere else, ever.
             */
            token: string;
            /** @description Non-secret leading characters of the token, for later identification. */
            token_prefix: string;
        };
        CreateGuardrailAttachmentRequest: {
            /** Format: uuid */
            scope_id?: string | null;
            scope_type: components["schemas"]["ScopeType"];
        };
        /**
         * @description A new engine instance.
         *
         *     `config` is a document rather than flattened fields because
         *     `deny_unknown_fields` would otherwise make every kind's schema part of this
         *     request's schema.
         */
        CreateGuardrailEngineRequest: {
            config?: Record<string, never>;
            display_name: string;
            enabled?: boolean;
            engine_key: string;
            kind: components["schemas"]["EngineKind"];
        };
        CreateGuardrailPolicyRequest: {
            enabled?: boolean;
            engine_ids: string[];
            failure_stance?: components["schemas"]["FailureStance"];
            mode?: components["schemas"]["Mode"];
            name: string;
        };
        /** @description Request to configure a federated sign-in provider. */
        CreateIdentityProviderRequest: {
            /** @description Scopes requested beyond the built-in OIDC ones. */
            additional_scopes?: string[];
            /** @description Email domains allowed to create accounts; empty means unrestricted. */
            allowed_email_domains?: string[];
            /** @description OAuth client identifier this gateway presents to the issuer. */
            client_id: string;
            /**
             * Format: password
             * @description OAuth client secret. Write-only: no operation reads it back.
             */
            client_secret: string;
            /** @description Claim read for group-to-role mapping. */
            groups_claim: string;
            /** @description Exact OIDC issuer URL. Must be `https`; it is the trust root. */
            issuer: string;
            /** @description Whether an unmatched verified identity may create an account at sign-in. */
            jit_enabled?: boolean;
            /** @description Administrator-facing name, shown on the sign-in page. */
            name: string;
        };
        /** @description Request to create an installation-owned endpoint. */
        CreateMcpEndpointRequest: {
            /** @description Connected apps to expose from the start. */
            app_ids?: string[];
            /** @description Whether any authorized client may invoke, or only attested tool calls. */
            execution_mode: components["schemas"]["McpExecutionMode"];
            /** @description Administrator-facing name, 1 to 120 characters. */
            name: string;
            /** @description Slug the endpoint will answer on. */
            slug: string;
        };
        /**
         * @description Request to add a model to the catalog.
         *
         *     Rates are decimal strings, not numbers: binary floating point cannot
         *     represent every rate exactly, and a silently rounded price bills real money
         *     incorrectly.
         */
        CreateModelRequest: {
            /** @description One-hour cache-write rate. */
            cache_write_1h_price_per_million?: string | null;
            /** @description Five-minute cache-write rate. */
            cache_write_5m_price_per_million?: string | null;
            /** @description Cache-read rate. Omit to bill cached input at the input rate. */
            cached_input_price_per_million?: string | null;
            /**
             * @description Canonical model identity: which underlying model this is, independent
             *     of the serving provider. Same charset as `gateway_id`. Omit for the
             *     common single-source case and it defaults to the gateway ID; set it
             *     when a source-qualified gateway ID (`kimi-k3-together`,
             *     `anthropic-us-claude-opus-5`) serves a model another route also serves.
             * @example gpt-5.6-sol
             */
            canonical_model_id?: string | null;
            /**
             * Format: int32
             * @description Context window in tokens.
             *
             *     Leave null when the publisher has not stated one. Do not estimate: this
             *     value is served to clients, which size their own token budgets from it,
             *     and a guess becomes a truncated request. Bedrock and Vertex require it.
             */
            context_window?: number | null;
            /**
             * @description Administrator-facing name.
             * @example GPT-5.6 Sol
             */
            display_name: string;
            /** @description Fast-tier one-hour cache-write rate. */
            fast_cache_write_1h_price_per_million?: string | null;
            /** @description Fast-tier five-minute cache-write rate. */
            fast_cache_write_5m_price_per_million?: string | null;
            /** @description Fast-tier cache-read rate. Omit to bill fast cached input at the fast input rate. */
            fast_cached_input_price_per_million?: string | null;
            /**
             * @description Premium-tier input rate, covering Anthropic fast mode and `OpenAI` priority
             *     processing alike. Fast input and output are all-or-nothing, and a fast cache rate
             *     requires a fast input rate: the fast rates never fall back to the standard ones,
             *     because a premium tier costs more and reading across understates it.
             */
            fast_input_price_per_million?: string | null;
            /** @description Fast-tier output rate in US dollars per million tokens. */
            fast_output_price_per_million?: string | null;
            /** @description Flex-tier one-hour cache-write rate. */
            flex_cache_write_1h_price_per_million?: string | null;
            /** @description Flex-tier five-minute cache-write rate. */
            flex_cache_write_5m_price_per_million?: string | null;
            /** @description Flex-tier cache-read rate. Omit to bill flex cached input at the flex input rate. */
            flex_cached_input_price_per_million?: string | null;
            /**
             * @description Discount-tier input rate, covering `OpenAI` flex processing. Flex input and output
             *     are all-or-nothing, and a flex cache rate requires a flex input rate: the flex
             *     rates never fall back to the standard ones, because a discount tier costs less and
             *     reading across overstates it.
             */
            flex_input_price_per_million?: string | null;
            /** @description Flex-tier output rate in US dollars per million tokens. */
            flex_output_price_per_million?: string | null;
            /**
             * @description Identifier clients will request. Lowercase letters, digits, and `._:-`.
             * @example gpt-5.6-sol
             */
            gateway_id: string;
            /**
             * @description Input rate in US dollars per million tokens, e.g. `"1.25"`.
             *
             *     Input and output rates are all-or-nothing: supply both or neither. Any
             *     cache rate requires an input rate to fall back to.
             */
            input_price_per_million?: string | null;
            /**
             * Format: int32
             * @description Input token count above which estimates apply the 2× input/cache and
             *     1.5× output long-context surcharge. Omit when the publisher has no such
             *     rule.
             */
            long_context_input_threshold?: number | null;
            /**
             * Format: int32
             * @description Maximum output tokens per response. Leave null when unstated; never guess.
             */
            max_output_tokens?: number | null;
            /** @description Output rate in US dollars per million tokens. */
            output_price_per_million?: string | null;
            /**
             * Format: uuid
             * @description Provider that will serve this model.
             */
            provider_id: string;
            /**
             * @description How this route binds requests to provider-provisioned capacity.
             *
             *     Omit this to preserve the provider's ordinary behavior. `provider_observed`
             *     and `metered_only` apply to `OpenAI`; `route_resource` and `metered_only`
             *     apply to Bedrock; `dedicated_only` and `metered_only` apply to Vertex.
             */
            provisioned_capacity_mode?: components["schemas"]["ProvisionedCapacityMode"];
            /**
             * @description Reasoning effort tokens this model's upstream accepts, in any order
             *     and deduplicated on persist. Omit to leave the ladder unstated — the
             *     upstream decides which requests it refuses; pass an empty array to
             *     record that the model takes no effort control.
             */
            supported_reasoning_efforts?: components["schemas"]["ReasoningEffort"][] | null;
            /** @description Whether to expose the model on the Anthropic Message Batches surface. */
            supports_anthropic_message_batches?: boolean;
            /**
             * @description Whether to expose the model on the Anthropic Messages surface.
             *
             *     The provider kind decides which surfaces are legal: Anthropic providers
             *     serve Messages only, `OpenAI` providers serve Responses only, Bedrock may
             *     serve either, and Vertex is decided by the publisher prefix of
             *     `upstream_id`. A combination the provider cannot serve is rejected with
             *     `protocol_contract`.
             */
            supports_anthropic_messages?: boolean;
            /** @description Whether to expose the model on the `OpenAI` Responses surface. */
            supports_openai_responses?: boolean;
            /** @description Whether the model supports tool calls. */
            supports_tools?: boolean;
            /** @description Whether the model accepts image input. */
            supports_vision?: boolean;
            /** @description Ultrafast-tier one-hour cache-write rate. */
            ultrafast_cache_write_1h_price_per_million?: string | null;
            /** @description Ultrafast-tier five-minute cache-write rate. */
            ultrafast_cache_write_5m_price_per_million?: string | null;
            /** @description Ultrafast-tier cache-read rate. Omit to use the Ultrafast input rate. */
            ultrafast_cached_input_price_per_million?: string | null;
            /**
             * @description Contract input rate for `OpenAI` Ultrafast processing. Ultrafast input and output
             *     are all-or-nothing, and an Ultrafast cache rate requires an Ultrafast input rate.
             *     These rates never fall back to Fast or Standard because `OpenAI` publishes no
             *     public equivalence for this access-controlled tier.
             */
            ultrafast_input_price_per_million?: string | null;
            /** @description Ultrafast-tier output rate in US dollars per million tokens. */
            ultrafast_output_price_per_million?: string | null;
            /**
             * @description Exact identifier the upstream expects.
             * @example gpt-5.6-sol
             */
            upstream_id: string;
        };
        /** @description Request to create an account. */
        CreatePersonRequest: {
            /** @description Display name, when one is wanted. */
            display_name?: string | null;
            /**
             * @description Email address, unique across the installation.
             * @example ada@example.com
             */
            email: string;
            /**
             * Format: password
             * @description Local password. Required for `password`, rejected for `sso`.
             */
            password?: string | null;
            /** @description Installation role. */
            role: components["schemas"]["UserRole"];
            /** @description How this account signs in. */
            sign_in_mode: components["schemas"]["SignInMode"];
            /** @description Local username. Required for `password`, rejected for `sso`. */
            username?: string | null;
        };
        /**
         * @description Request to configure a new model provider.
         *
         *     Which fields are required depends on `provider_kind` and `cloud_identity_mode`;
         *     a field the selected combination does not use must be left out rather than
         *     sent empty. Rejections name the rule and the field, so correct that field
         *     rather than varying the others.
         */
        CreateProviderRequest: {
            /**
             * @description Provider API key, required when `auth_method` is `api_key`.
             *
             *     Encrypted before storage and never returned by any read.
             */
            api_key?: string | null;
            /**
             * @description How the gateway authenticates to it.
             *
             *     Bedrock and Vertex accept only `workload_identity`. Anthropic and
             *     `OpenAI`-shaped providers accept `api_key`, or `none` when the provider
             *     is served entirely by subscription credentials.
             */
            auth_method: components["schemas"]["ModelProviderAuthMethod"];
            cloud_identity_mode?: null | components["schemas"]["CloudIdentityMode"];
            /** @description Bedrock control-plane endpoint override. */
            control_endpoint_url?: string | null;
            /** @description Override for the upstream base URL. Absolute HTTPS, no path or query. */
            endpoint_url?: string | null;
            /** @description External ID matching the assumed role's trust policy, if it sets one. */
            external_id?: string | null;
            /** @description Google Cloud project, required for Vertex. */
            gcp_project_id?: string | null;
            /** @description Google Cloud project billed for quota, if different. */
            gcp_quota_project_id?: string | null;
            /**
             * @description Service account to impersonate, required by and only by
             *     `service_account_impersonation`.
             */
            gcp_service_account_email?: string | null;
            /** @description Bedrock Mantle endpoint override. */
            mantle_endpoint_url?: string | null;
            /**
             * @description Bedrock project this provider routes through. Defaults to `default`.
             *
             *     An installation that pins its approved projects rejects any other value
             *     with `bedrock_project_not_approved`.
             */
            mantle_project_id?: string | null;
            mantle_retention_policy?: null | components["schemas"]["BedrockDataRetentionPolicy"];
            /**
             * @description Administrator-facing name, up to 120 characters.
             * @example Production Bedrock
             */
            name: string;
            openai_compatible_api?: null | components["schemas"]["OpenAiCompatibleApi"];
            /** @description Which upstream this provider talks to. */
            provider_kind: components["schemas"]["ModelProviderKind"];
            /**
             * @description Cloud region, required for Bedrock and Vertex.
             *
             *     On an installation that pins its Bedrock projects, this is taken from the
             *     approved project rather than from the request.
             * @example us-east-1
             */
            region?: string | null;
            /**
             * @description Role to assume, required by and only by `assume_role`.
             * @example arn:aws:iam::123456789012:role/model-gateway
             */
            role_arn?: string | null;
        };
        /**
         * @description One policy to create. Scope and dimension fix which requests the policy
         *     sees and cannot be changed afterwards; the value and the switch can.
         */
        CreateRateLimitRequest: {
            /**
             * @description The quantity limited, which also fixes the unit of `limit_value`:
             *     requests in flight for `concurrency`, per-minute rates for
             *     `requests_per_minute` and `tokens_per_minute`. One policy per scope and
             *     dimension.
             */
            dimension: components["schemas"]["Dimension"];
            /**
             * @description Whether the policy is enforced. Defaults to true; a disabled policy is
             *     retained but admits everything.
             */
            enabled?: boolean;
            /**
             * Format: int64
             * @description The cap, in the unit `dimension` names. `0` denies every request in
             *     scope on that dimension rather than meaning "unlimited".
             */
            limit_value: number;
            /**
             * Format: uuid
             * @description The account, team, or gateway model this policy applies to — from
             *     `listPeople`, `listTeams`, or `listProviderModels` respectively. Null
             *     for `installation`, required for every other scope.
             */
            scope_id?: string | null;
            /**
             * @description What the policy covers. `installation` is the whole gateway; every other
             *     value names a target in `scope_id`. `connected_app` is reserved and
             *     refused until inference can be attributed to an app.
             */
            scope_type: components["schemas"]["RateLimitScopeType"];
        };
        /** @description Request to author an egress bundle. */
        CreateSandboxEgressBundleRequest: {
            /**
             * @description Exact-match tunnel domains, as bare strings (blind) or `{domain, mode}`
             *     objects. At least one.
             */
            domains?: components["schemas"]["TunnelDomainRequest"][];
            /**
             * @description Slug-shaped name: 1 to 63 lowercase letters, digits, and dashes, not
             *     starting with a dash.
             */
            name: string;
        };
        /** @description Request to author a sandbox profile. */
        CreateSandboxProfileRequest: {
            /** @description Connector-terminated connections this profile may use. */
            connector_connection_ids?: string[];
            /**
             * Format: int32
             * @description CPU ceiling in millicores, at least the request.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * @description The class a spawn gets when it names none; must be listed in
             *     `runner_classes`. Required exactly when that list is non-empty.
             */
            default_runner_class?: string | null;
            /**
             * @description Egress bundles this profile references, each required (always in the
             *     effective set) or optional (a spawn may add it by name).
             */
            egress_bundles?: components["schemas"]["EgressBundleReferenceRequest"][];
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: int32
             * @description Seconds of inactivity while the client remains in the foreground. Omit to
             *     keep the ordinary idle timer.
             */
            foreground_idle_timeout_seconds?: number | null;
            /**
             * Format: int32
             * @description Seconds of inactivity before the sandbox is stopped, 60 to 86400.
             */
            idle_timeout_seconds: number;
            /**
             * @description Digest-pinned workload image, ending `@sha256:` and 64 hex characters.
             * @example registry.example.test/sandbox@sha256:0000000000000000000000000000000000000000000000000000000000000000
             */
            image: string;
            /**
             * Format: int32
             * @description Pod incarnations one sandbox may consume, 1 to 16. Defaults to 3;
             *     1 disables reincarnation after infrastructure loss (ADR 0068 part 3).
             */
            max_pod_incarnations?: number;
            /**
             * Format: int32
             * @description Memory ceiling in MiB, at least the request.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /**
             * @description Slug-shaped name clients will select by: 1 to 63 lowercase letters,
             *     digits, and dashes, not starting with a dash.
             */
            name: string;
            /** @description Node placement selector object; omit for no constraint. */
            node_selector?: unknown;
            /** @description Whether a spawn may declare a pinned repository. Off by default. */
            permits_repositories?: boolean;
            /** @description Harnesses this profile permits. At least one. */
            permitted_harnesses: components["schemas"]["ConversationHarness"][];
            /**
             * @description Gateway model IDs this profile permits. Empty — the default — declares
             *     no model restriction, so every existing caller keeps its behavior.
             */
            permitted_model_routes?: string[];
            /**
             * @description Annotations rendered onto this profile's pods; omit for none.
             *
             *     A JSON object of string keys and string values. This is where a
             *     provisioner's "do not disrupt this node" annotation goes: a sandbox pod
             *     carries `restartPolicy: Never`, so a node consolidated out from under a
             *     running sandbox ends that run for good.
             */
            pod_annotations?: unknown;
            /** @description How the workload is started. Defaults to `first_party`. */
            profile_runtime?: components["schemas"]["SandboxProfileRuntime"];
            /**
             * @description Runner classes this profile permits, by catalogue name (ADR 0109).
             *     Empty keeps the profile on its own inline size, and `default_runner_class`
             *     must be omitted with it.
             */
            runner_classes?: string[];
            /**
             * @description Hardened runtime class the pod runs under.
             * @example gvisor
             */
            runtime_class: string;
            /**
             * Format: int64
             * @description Per-sandbox ceiling in micro-US-dollars of shadow model cost (all billing classes).
             */
            spend_ceiling_microusd: number;
            /**
             * @description Taints this profile's pods tolerate; omit to tolerate none.
             *
             *     A JSON array of Kubernetes tolerations, each naming the taint `key` it
             *     tolerates and an `operator` of `Exists` or `Equal`. A dedicated node
             *     pool for a hardened runtime is normally tainted, so a profile with no
             *     toleration is one the scheduler will never place there.
             */
            tolerations?: unknown;
            /**
             * @description Exact-match tunnel domains, as bare strings (blind, the original
             *     contract) or `{domain, mode}` objects. Empty by default, and empty is
             *     the safe value.
             */
            tunnel_domains?: components["schemas"]["TunnelDomainRequest"][];
            /**
             * Format: int32
             * @description Total seconds a sandbox may live, at least the idle timeout.
             */
            wall_clock_timeout_seconds: number;
            /** @description Repository reference whose warm layer to refresh. Omit for cold starts. */
            warm_image_reference?: string | null;
            /**
             * @description Schedule for refreshing the warm image. Required with the reference, and
             *     refused without it.
             */
            warm_image_schedule?: string | null;
        };
        /** @description Request to author a runner class. */
        CreateSandboxRunnerClassRequest: {
            /**
             * Format: int32
             * @description CPU ceiling in millicores, at least the request.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: int32
             * @description Memory ceiling in MiB, at least the request.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /**
             * @description Slug-shaped name: 1 to 63 lowercase letters, digits, and dashes, not
             *     starting with a dash.
             */
            name: string;
            /**
             * @description Node selector object naming the pool that has machines of this size;
             *     omit for no placement constraint of its own.
             */
            node_selector?: unknown;
            /**
             * Format: int32
             * @description Smallest-to-largest order, 0 to 1,000,000; unique.
             */
            rank: number;
            /**
             * Format: int32
             * @description Optional wall-clock default in seconds, 60 to 604800.
             */
            wall_clock_timeout_seconds?: number | null;
        };
        /** @description Request to create a SCIM connector. */
        CreateScimConnectorRequest: {
            /** @description Administrator-facing name, unique across connectors. */
            name: string;
        };
        /** @description Request to create a service identity. */
        CreateServiceIdentityRequest: {
            /** @description Display name, when one is wanted. */
            display_name?: string | null;
            /**
             * Format: uuid
             * @description Owning team. Membership is the identity's ownership, its app access,
             *     and its spend-attribution container; an identity holds exactly one.
             */
            team_id: string;
            /**
             * @description Stable handle: lowercase letters, digits, and dashes, starting with a
             *     letter or digit, at most 63 characters.
             * @example shipright
             */
            username: string;
        };
        /** @description Request to create a shared app and its first revision. */
        CreateSharedAppRequest: {
            /** @description Authored bundle bytes, base64-encoded. Up to 1 MiB decoded. */
            bundle_base64?: string | null;
            /** @description Publishing harness, recorded as content-free provenance. */
            client_name?: string | null;
            /** @description The manifest pinning what revision 1 may call. */
            manifest: components["schemas"]["SharedAppManifest"];
            /**
             * @description Human-readable name, 1 to 120 characters. Refused rather than trimmed
             *     to fit, and bounded at the column that stores it.
             */
            name: string;
            /** @description Slug, unique among this author's apps. Derived from `name` when absent. */
            slug?: string | null;
        };
        /** @description Request to append a revision. */
        CreateSharedAppRevisionRequest: {
            /** @description Authored bundle bytes, base64-encoded. Up to 1 MiB decoded. */
            bundle_base64?: string | null;
            /** @description Publishing harness, recorded as content-free provenance. */
            client_name?: string | null;
            /** @description The manifest pinning what the new revision may call. */
            manifest: components["schemas"]["SharedAppManifest"];
        };
        /** @description Request to create a team. */
        CreateTeamRequest: {
            /**
             * @description Human-readable name, unique case-insensitively.
             * @example Platform
             */
            name: string;
            /**
             * @description Stable slug: lowercase letters, digits, and hyphens.
             * @example platform
             */
            slug: string;
        };
        /**
         * @description Location where a governed REST credential is injected.
         * @enum {string}
         */
        CredentialPlacement: "none" | "authorization_bearer" | "authorization_basic" | "header";
        /**
         * @description Versioned recipe used to resolve and apply connected-app credentials.
         * @enum {string}
         */
        CredentialScheme: "opaque_shared_secret" | "static_bearer" | "static_header" | "datadog_api_and_application_keys" | "datadog_access_token" | "delegated_oauth" | "git_http_basic" | "github_app" | "gateway_act_as_caller";
        /**
         * @description What still points at a deleted app, split the way the purge refusal splits
         *     it.
         *
         *     The first four are recorded history: kept deliberately, for as long as the
         *     record exists, and no control anywhere removes it — an app any of them
         *     reference stays a deleted row. The last is live configuration an
         *     administrator can detach, after which the purge can be retried.
         */
        DeletedConnectedAppHoldersView: {
            /**
             * Format: int64
             * @description Recorded outbound HTTP requests the app made. History.
             */
            app_http_events: number;
            /**
             * Format: int64
             * @description MCP endpoints the app is still assigned to. Configuration, detachable.
             */
            mcp_endpoint_assignments: number;
            /**
             * Format: int64
             * @description Recorded runtime executions the app was authorized for. History.
             */
            runtime_executions: number;
            /**
             * Format: int64
             * @description Sandboxes recorded as having cloned through the app. History.
             */
            sandboxes: number;
            /**
             * Format: int64
             * @description Recorded tool calls made through the app. History.
             */
            tool_call_events: number;
        };
        /** @description Listing of the apps a delete left behind. */
        DeletedConnectedAppListResponse: {
            /** @description Deleted apps, most recently deleted first. */
            data: components["schemas"]["DeletedConnectedAppView"][];
        };
        /**
         * @description A connected app a delete left behind.
         *
         *     Deleting an app is a soft delete: the row stays so the recorded calls made
         *     through it keep resolving to something. What it no longer holds is its
         *     identifiers — a deleted app's name and tool slug are free for a new app to
         *     take. This view exists so that the rows a soft delete leaves are visible at
         *     all, since `listConnectedApps` cannot show them without meaning something
         *     different by "connected app".
         */
        DeletedConnectedAppView: {
            /** @description When it was deleted, RFC 3339. */
            deleted_at: string;
            /** @description What still references the app, and how much of it. */
            held_by: components["schemas"]["DeletedConnectedAppHoldersView"];
            /**
             * Format: uuid
             * @description Stable app ID, and what a purge names.
             */
            id: string;
            /** @description Which adapter served it. */
            kind: components["schemas"]["ConnectedAppKind"];
            /** @description The name it carried when it was deleted. */
            name: string;
            /**
             * @description Whether `purgeConnectedApp` would succeed right now — nothing recorded
             *     or configured references the app.
             *
             *     Precomputed here so a caller can tell a purgeable row from a retained
             *     one without attempting the purge. A snapshot, not a reservation: the
             *     purge re-counts under a lock and remains the authority, so a purge can
             *     still be refused if a reference lands between this read and the call.
             */
            purgeable: boolean;
            /** @description The slug it exposed tools under, when it had a projection. */
            tool_slug?: string | null;
        };
        /** @description Request to delete one sandbox WIP ref. */
        DeleteGitForgeWipRefRequest: {
            /** @description Branch name, which must start `mg-wip/`. */
            name: string;
            /** @description `owner/repo` as the forge names it. */
            repository: string;
        };
        /** @enum {string} */
        Dimension: "concurrency" | "requests_per_minute" | "tokens_per_minute";
        /**
         * @description What to do with Direct MCP endpoints a policy change would strand.
         * @enum {string}
         */
        DirectEndpointResolution: "reject" | "switch_to_gateway_attested" | "remove_assignments";
        /**
         * @description One model the upstream reported, normalized across provider kinds.
         *
         *     Bedrock and Vertex describe their catalogs differently, and an agent asking
         *     "what can I import" should not have to branch on which cloud answered. The
         *     fields kept are the ones [`CreateModelRequest`] needs; everything the admin
         *     page uses for presentation is left out.
         */
        DiscoveredModelView: {
            /** @description What to do about `access_state`, in prose. */
            access_detail: string;
            /**
             * @description Stable slug for why the model is or is not usable.
             *
             *     `ready` for a model that can serve now. The rest name what is missing:
             *     Bedrock reports `adapter_review`, `profile_required`, `retention_setup`,
             *     `retention_blocked`, `policy_blocked`, `action_required`, `unverified`,
             *     and `inactive`; Vertex reports `action_required` and `location_required`.
             */
            access_state: string;
            /**
             * Format: int32
             * @description Context window in tokens, or null when the upstream does not state one.
             *
             *     Bedrock's model listing carries no limits, so this is null for every
             *     Bedrock model. Null means unknown, not unlimited: look the value up in
             *     the publisher's documentation rather than estimating it, because clients
             *     size their token budgets from what the catalog serves.
             */
            context_window?: number | null;
            /** @description Publisher-facing name. */
            display_name: string;
            /**
             * @description Whether this model can be added to the catalog as it stands.
             *
             *     False means something about the account, not the request, has to change
             *     first — see `access_state`. Creating the model anyway is not blocked, but
             *     it will not serve traffic.
             */
            importable: boolean;
            /** @description Publisher lifecycle stage, verbatim from the upstream. */
            lifecycle: string;
            /**
             * Format: int32
             * @description Maximum output tokens per response, null when unstated. Never estimate.
             */
            max_output_tokens?: number | null;
            /** @description Why cache dimensions remain absent when standard rates resolved. */
            published_cache_pricing_detail?: string | null;
            /** @description Published one-hour cache-write rate, when exactly identified. */
            published_cache_write_1h_price_per_million?: string | null;
            /** @description Published five-minute cache-write rate, when exactly identified. */
            published_cache_write_5m_price_per_million?: string | null;
            /** @description Published cache-read rate, when AWS exposes an exact matching dimension. */
            published_cached_input_price_per_million?: string | null;
            /**
             * @description Published input rate in US dollars per million tokens, when the upstream
             *     publishes one for this model in this region.
             *
             *     Present for Bedrock models the gateway holds a published rate for, null
             *     everywhere else. It is the vendor's list price, not the installation's
             *     negotiated rate, and it is offered so that `createProviderModel` can be
             *     called with a real figure rather than an invented one — not as a value to
             *     pass through unread.
             */
            published_input_price_per_million?: string | null;
            /** @description Published output rate, on the same terms as the input rate. */
            published_output_price_per_million?: string | null;
            /** @description Date the published rates were last confirmed, `YYYY-MM-DD`. */
            published_price_effective_date?: string | null;
            published_scheduled_cache_write_1h_price_per_million?: string | null;
            published_scheduled_cache_write_5m_price_per_million?: string | null;
            published_scheduled_cached_input_price_per_million?: string | null;
            published_scheduled_input_price_per_million?: string | null;
            published_scheduled_output_price_per_million?: string | null;
            /** @description Future AWS price boundary, when the Price List already publishes one. */
            published_scheduled_price_effective_date?: string | null;
            /**
             * @description Which underlying model this is, independent of the serving route or
             *     speed tier. Set when the upstream has a source-qualified variant (e.g.
             *     a Fireworks fast router) that shares an identity with another model.
             *     Null means the gateway ID itself is the canonical identity.
             */
            suggested_canonical_model_id?: string | null;
            /**
             * @description Vendor-first gateway ID the gateway suggests for this model.
             *
             *     Only a suggestion, but a considered one: Claude Code silently drops
             *     discovered models whose ID starts with neither `claude` nor `anthropic`,
             *     so a name chosen freehand can leave the model invisible in that client's
             *     picker.
             */
            suggested_gateway_id: string;
            /**
             * @description Reasoning effort tokens the gateway's published-ladder table records
             *     for this model's canonical identity. Null means no published ladder —
             *     the field is a prefill for the create form, never an upstream fact.
             */
            suggested_reasoning_efforts?: components["schemas"]["ReasoningEffort"][] | null;
            /** @description Whether the model can serve the Anthropic Message Batches surface. */
            supports_anthropic_message_batches: boolean;
            /** @description Whether the model can serve the Anthropic Messages surface. */
            supports_anthropic_messages: boolean;
            /** @description Whether the model can serve the `OpenAI` Responses surface. */
            supports_openai_responses: boolean;
            /** @description Whether the model supports tool calls. */
            supports_tools: boolean;
            /** @description Whether the model accepts image input. */
            supports_vision: boolean;
            /** @description Exact identifier the upstream expects, ready for `upstream_id`. */
            upstream_id: string;
        };
        /** @description One ceiling operation, judged for the chosen principal. */
        EffectiveOperationView: {
            /** @description Operation identifier from the app's ceiling. */
            operation_id: string;
            /** @description Whether the principal may invoke it. */
            permitted: boolean;
            /** @description `denied` (a deny matched) or `outside_allow_set`, when refused. */
            refusal?: string | null;
            refused_by_scope?: null | components["schemas"]["AccessPolicyScopeKind"];
        };
        /** @description Egress-bundle listing. */
        EgressBundleListResponse: {
            /** @description Bundles in name order. */
            data: components["schemas"]["EgressBundleView"][];
        };
        /** @description One bundle reference in a profile write, and on what terms. */
        EgressBundleReferenceRequest: {
            /** @description Bundle name in the installation catalogue. */
            name: string;
            /**
             * @description `true` (the default): always in the profile's effective set. `false`:
             *     a spawn may add it by naming it.
             */
            required?: boolean;
        };
        /**
         * @description One operator-owned egress bundle: a named set of `(domain, mode)` entries
         *     (ADR 0109, modes per ADR 0072).
         */
        EgressBundleView: {
            /** @description Every domain in the bundle, in stable order, each with its egress mode. */
            domains: components["schemas"]["TunnelDomainView"][];
            /**
             * Format: uuid
             * @description Bundle ID.
             */
            id: string;
            /** @description Slug-shaped name profiles and spawns select by. */
            name: string;
            /** @description Live profiles currently referencing this bundle, by name. */
            profile_names: string[];
        };
        EnabledRequest: {
            enabled: boolean;
        };
        /** @description MCP endpoints a policy change carried with it. */
        EndpointCascade: {
            /** @description Direct endpoints the app was removed from. */
            removed_endpoint_ids: string[];
            /** @description Endpoints moved from Direct to Gateway-Attested. */
            switched_endpoint_ids: string[];
        };
        /**
         * @description The code-owned implementations an engine instance can name.
         *
         *     A closed enum rather than a free string so an unrecognized kind is refused
         *     by name at the API boundary instead of reaching storage and becoming a row
         *     that only fails when a request is admitted. Every kind here is in-process;
         *     the external kinds the schema reserves (`webhook`, the managed adapters)
         *     keep their ADR 0055 sequencing and are absent by name until they land.
         * @enum {string}
         */
        EngineKind: "secrets" | "pii" | "term_list";
        /** @description Inference spend split by whether the caller was a sandbox. */
        ExecutionKindUsageView: {
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /** Format: int64 */
            credits_cost_microusd: number;
            /** Format: int64 */
            estimated_cost_microusd: number;
            execution_kind: string;
            /** Format: int64 */
            inference_requests: number;
            /** Format: int64 */
            input_tokens: number;
            /** Format: int64 */
            output_tokens: number;
            /** Format: int64 */
            pending_rate_requests: number;
            /** Format: int64 */
            priced_requests: number;
            /** Format: int64 */
            provisional_requests: number;
            /** Format: int64 */
            provisioned_cost_microusd: number;
            /** Format: int64 */
            subscription_cost_microusd: number;
            /** Format: int64 */
            unpriceable_requests: number;
        };
        /**
         * @description Which failover regime a gateway model follows (ADR 0113).
         * @enum {string}
         */
        FailoverPolicyView: "managed" | "custom";
        /** @description The installation-wide failover default (ADR 0113). */
        FailoverRoutingView: {
            /**
             * @description Whether managed models derive a waterfall from the enabled routes
             *     sharing their canonical model ID — direct providers first, then
             *     hyperscalers, then aggregators. Off, a managed model serves from its
             *     own route only.
             */
            automatic: boolean;
            /**
             * @description The trigger every derived waterfall fails over on. False, the default,
             *     fails over only on outages.
             */
            trigger_on_rate_limit: boolean;
            /** @description When the setting last changed, RFC 3339. */
            updated_at: string;
        };
        /** @enum {string} */
        FailureStance: "fail_open" | "fail_closed";
        /** @description One failed validation rule. */
        FieldError: {
            /**
             * @description Stable machine-readable rule identifier.
             * @example model_limit_order
             */
            code: string;
            /**
             * @description Request field the rule applies to, in JSON pointer-free dotted form.
             * @example context_window
             */
            field: string;
            /** @description Human-readable explanation of what to change. */
            message: string;
        };
        /** @enum {string} */
        FindingPatternType: "category" | "preview_prefix";
        /** @enum {string} */
        FindingReviewReason: "false_positive" | "test_fixture" | "accepted_risk" | "fixed" | "other";
        /** @enum {string} */
        FindingReviewStatus: "open" | "dismissed";
        /**
         * @description Authenticated client identity attached to gateway activity.
         * @enum {string}
         */
        GatewayClient: "modelctl" | "claude-code" | "claude-desktop" | "codex" | "pi" | "opencode" | "omp" | "grok-build" | "tidebreak" | "tidewatch" | "generic" | "personal-token";
        /**
         * @description The git egress and kind-declared REST channel a forge is brokered through.
         *
         *     Deliberately not a variation of [`RestApiConnectionInput`]. A forge carries
         *     no operation catalog — git's protocol endpoints are declared in code, so
         *     there is nothing here for an administrator to widen — and no injection
         *     header, because git authenticates with `Authorization`. The agent access
         *     mode applies to the compiled buffered REST channel only. Its ceilings bound
         *     a streamed packfile and are correspondingly wider; a bound outside its
         *     range is refused, never narrowed to fit.
         */
        GitForgeConnectionInput: {
            agent_access_mode?: null | components["schemas"]["ConnectedAppAccessMode"];
            /** @description Whether the forge origin may resolve to a private-network address. */
            allow_private_networks?: boolean;
            credential_mode?: null | components["schemas"]["GitForgeCredentialMode"];
            credential_placement?: null | components["schemas"]["CredentialPlacement"];
            /**
             * Format: int64
             * @description Non-secret identifier of the GitHub App to mint from.
             *
             *     For `github_app` mode only, and optional there: supply it alongside a
             *     private key when the app already exists, or leave both out and let the
             *     console create the app on GitHub. Refused in `static_token` mode.
             */
            github_app_id?: number | null;
            /**
             * Format: int64
             * @description Largest streamed request body in bytes, 1024 to 68719476736. Defaults to 2147483648.
             */
            max_request_bytes?: number | null;
            /**
             * Format: int64
             * @description Largest streamed response body in bytes, 1024 to 68719476736. Defaults to 2147483648.
             */
            max_response_bytes?: number | null;
            /**
             * Format: int32
             * @description End-to-end timeout in seconds, 1 to 3600. Defaults to 900.
             */
            timeout_seconds?: number | null;
            /** @description Slug the runtime egress route addresses the forge by. Unique across apps. */
            tool_slug?: string | null;
        };
        /**
         * @description Where one git-forge app's push credential comes from.
         *
         *     The distinction is lifecycle, not transport: both modes reach the forge
         *     as an HTTP basic credential over git smart HTTP, and the sidecar, the pod,
         *     and the wire format are identical either way. What differs is whether the
         *     installation stores a credential a human created and must remember to
         *     rotate, or an identity the gateway mints an hour-long credential from on
         *     each use.
         * @enum {string}
         */
        GitForgeCredentialMode: "static_token" | "github_app" | "delegated_oauth" | "github_app_installation";
        /** @description Listing of a forge's approved installations. */
        GitForgeGithubAppInstallationListResponse: {
            /** @description Every installation the forge reported, empty before anyone approves one. */
            data: components["schemas"]["GitForgeGithubAppInstallationView"][];
        };
        /** @description One approved installation of a forge's GitHub App. */
        GitForgeGithubAppInstallationView: {
            /** @description Account the app is installed on. */
            account_login?: string | null;
            /**
             * Format: int64
             * @description Identifier a token would be minted for.
             */
            id: number;
            /** @description Whether the install covers every repository or a chosen set. */
            repository_selection?: string | null;
        };
        /** @description One requested permission an installation has not accepted. */
        GitForgeGithubAppPermissionView: {
            /** @description The level the installation carries, absent when it carries none. */
            granted?: string | null;
            /** @description GitHub's own name for the permission. */
            name: string;
            /** @description The level this gateway's manifest asks for. */
            required: string;
        };
        /**
         * @description The GitHub App one git forge mints installation tokens from.
         *
         *     Everything here is public information about the app or non-secret
         *     configuration pointing at it. The private key is represented only by
         *     `credential_configured`, exactly as `credential_configured` on the app view
         *     represents every other stored secret.
         */
        GitForgeGithubAppView: {
            /** @description Whether a signing key is stored for this forge. */
            credential_configured: boolean;
            /**
             * Format: int64
             * @description Non-secret app identifier, absent until the app has been made.
             */
            github_app_id?: number | null;
            /** @description The app's own page on the forge, when the forge was asked. */
            html_url?: string | null;
            /** @description Page a human approves an installation of this app on. */
            install_url?: string | null;
            /**
             * Format: int64
             * @description Installation the forge mints for, absent until one is chosen.
             */
            installation_id?: number | null;
            /** @description Settings page where an owner changes the installation's repository access. */
            installation_settings_url?: string | null;
            /**
             * @description When this gateway process last minted a token, if it has.
             *
             *     Per-process: minted tokens live in memory and are never stored, so a
             *     replica that has not minted since it started reports nothing. Absent is
             *     "no evidence here", never "broken".
             */
            last_minted_at?: string | null;
            /** @description Administrator-facing app name, when the forge was asked. */
            name?: string | null;
            /**
             * @description Whether the installation's accepted permissions could be read at all.
             *
             *     False means the question went unasked — no installation chosen, no
             *     signing key, or the forge was unreachable — and `unmet_permissions`
             *     below says nothing. It is deliberately not conflated with an empty
             *     unmet list, which is the positive answer that the installation is
             *     current.
             */
            permissions_observed: boolean;
            /** @description Page an owner accepts the new permissions on, when one can be addressed. */
            permissions_update_url?: string | null;
            /** @description Slug the app's pages are addressed by. */
            slug?: string | null;
            /**
             * @description Permissions the manifest asks for that this installation does not carry.
             *
             *     Non-empty is the expected state after a release widens the manifest, not
             *     a fault: a manifest edit cannot grow an App that already exists
             *     (ADR 0052), so the grant only moves when an owner re-approves.
             */
            unmet_permissions: components["schemas"]["GitForgeGithubAppPermissionView"][];
        };
        /**
         * @description How one principal reaches a git-forge app, for the one-per-origin choice.
         *
         *     Reachability is a union, but where exactly one forge app must serve an
         *     origin (a repository spawn, a hosted-machine credential mint) the
         *     directly granted app outranks every team-inherited one, and ambiguity
         *     is judged only among the top rank (ADR 0107 decision 3). A person is
         *     always `team`; only a service identity can hold an app directly.
         * @enum {string}
         */
        GitForgeGrantSource: "identity" | "team";
        /** @description Listing of repositories the forge installation covers. */
        GitForgeInstallationRepositoryListResponse: {
            /** @description First page of repositories the installation token can see. */
            data: components["schemas"]["GitForgeInstallationRepositoryView"][];
        };
        /** @description One repository the installation token can see. */
        GitForgeInstallationRepositoryView: {
            /** @description `owner/repo`. */
            full_name: string;
            /** @description Whether the forge treats the repository as private. */
            private: boolean;
        };
        /**
         * @description Convention the login half of a `git_http_basic` credential follows.
         *
         *     Forges key a token's meaning off the login it arrives with, and the
         *     conventions are few and fixed: GitHub reads `x-access-token` as "the
         *     password is an App installation or fine-grained token", GitLab reads
         *     `oauth2` as "the password is an OAuth access token", and a plain bot
         *     login means the password is that account's own token. Naming the
         *     convention rather than accepting free text is what lets a future
         *     credential recipe — one that mints short-lived installation tokens, say —
         *     arrive as a new variant instead of a reinterpretation of whatever
         *     administrators happened to type.
         * @enum {string}
         */
        GitForgeLogin: "x_access_token" | "oauth2" | "bot_user";
        /** @description Listing of sandbox WIP refs on one repository. */
        GitForgeWipRefListResponse: {
            /** @description Every admitted `mg-wip/*` ref the forge reported. */
            data: components["schemas"]["GitForgeWipRefView"][];
        };
        /** @description One `mg-wip/*` branch the forge currently holds. */
        GitForgeWipRefView: {
            /** @description Committer timestamp of that object, when the forge served a commit. */
            committed_at?: string | null;
            /** @description First line of the commit message, when the forge served a commit. */
            message?: string | null;
            /** @description Branch name, always starting `mg-wip/`. */
            name: string;
            /** @description Object SHA the ref currently names. */
            sha: string;
        };
        /**
         * @description One aggregate keyed by the exact dimensions requested in `group_by`.
         *
         *     Reports the same committed-attempt efficiency statistics as the by-model
         *     and by-client rollups, at whatever grain was asked for, so narrowing a
         *     question to a window, a filter, or a cross-tabulation does not cost the
         *     answer. One denominator differs and is stated rather than left to be
         *     discovered: token and cost sums count every attempt, abandoned failovers
         *     included, while the latency, context, and message statistics count
         *     committed attempts only — `committed_requests` is the denominator for
         *     those.
         */
        GroupedInferenceUsageView: {
            /**
             * Format: int64
             * @description Abandoned failover attempts.
             */
            abandoned_requests: number;
            /** Format: int64 */
            attributed_requests: number;
            /**
             * Format: int64
             * @description Mean committed-request duration in milliseconds.
             */
            avg_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Mean committed-request time-to-first-byte in milliseconds.
             */
            avg_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Mean northbound message count across committed requests that recorded one.
             */
            avg_message_count?: number | null;
            /**
             * Format: double
             * @description Mean declared tool-definition count across committed requests that recorded one.
             */
            avg_tool_definition_count?: number | null;
            /**
             * Format: double
             * @description Mean user-message count across committed requests that recorded one.
             */
            avg_user_message_count?: number | null;
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Committed requests (`attempt_role` is not abandoned) — the denominator
             *     of every statistic below.
             */
            committed_requests: number;
            /** Format: int64 */
            credits_cost_microusd: number;
            /** Format: int64 */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Cache-write input tokens on committed first-turn requests.
             */
            first_turn_cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Estimated billed cost of committed first-turn requests, in
             *     micro-US-dollars.
             */
            first_turn_cost_microusd: number;
            /**
             * Format: int64
             * @description Input tokens on committed first-turn requests.
             */
            first_turn_input_tokens: number;
            /**
             * Format: int64
             * @description Committed first-turn requests: one user message and nothing else in
             *     the transcript beyond the standing instructions block — the opening
             *     request of a conversation, never a tool-loop continuation. What
             *     starting a session costs — system prompt, standing instruction files,
             *     and tool definitions land here with nothing yet amortized.
             */
            first_turn_requests: number;
            /** Format: int64 */
            inference_requests: number;
            /** Format: int64 */
            input_tokens: number;
            /** @description Dimension names mapped to recorded values. Historical nulls remain null. */
            key: Record<string, never>;
            last_activity_at?: string | null;
            /** Format: int64 */
            output_tokens: number;
            /**
             * Format: int64
             * @description Median committed-request duration in milliseconds.
             */
            p50_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Median committed-request time-to-first-byte in milliseconds.
             */
            p50_first_byte_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request duration in milliseconds.
             */
            p95_duration_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request time-to-first-byte in milliseconds.
             */
            p95_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Peak input-tokens / context-window across committed requests.
             */
            peak_context_utilization?: number | null;
            /** Format: int64 */
            pending_rate_requests: number;
            /** Format: int64 */
            priced_requests: number;
            /** Format: int64 */
            provisional_requests: number;
            /** Format: int64 */
            provisioned_cost_microusd: number;
            /**
             * Format: int64
             * @description Committed requests that recorded request-shape counts — the denominator
             *     for the first-turn statistics. History that predates shape capture has
             *     no `user_message_count` and is excluded from them, not counted as zero.
             */
            shape_recorded_requests: number;
            /** Format: int64 */
            subscription_cost_microusd: number;
            /** Format: int64 */
            subscription_requests: number;
            /** Format: int64 */
            unattributed_requests: number;
            /** Format: int64 */
            unpriceable_requests: number;
        };
        GuardrailActivityResponse: {
            data: components["schemas"]["GuardrailActivityView"][];
        };
        GuardrailActivityView: {
            /** Format: int64 */
            blocked: number;
            day: string;
            /** Format: int64 */
            degraded: number;
            engine_key: string;
            /** Format: int64 */
            evaluations: number;
            /** Format: int64 */
            flagged: number;
            /**
             * @description Requested scope dimensions mapped to their recorded values. Present
             *     only when `group_by` was requested; a recorded null remains null.
             */
            key?: Record<string, never>;
            /** Format: uuid */
            policy_id?: string | null;
            policy_name: string;
            /**
             * @description Brokered surface these evaluations ran on: `inference_request`,
             *     `tool_call`, `tool_result`, or `app_egress`.
             */
            surface: string;
        };
        GuardrailAttachmentResponse: {
            data: components["schemas"]["GuardrailAttachmentView"];
        };
        GuardrailAttachmentView: {
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            scope_id?: string | null;
            scope_label: string;
            scope_type: string;
        };
        GuardrailEffectiveModeView: {
            /** @description Mode the gateway actually applies on this surface. */
            mode: string;
            /** @description Brokered surface this mode applies to. */
            surface: string;
        };
        GuardrailEngineListResponse: {
            data: components["schemas"]["GuardrailEngineView"][];
        };
        GuardrailEngineResponse: {
            data: components["schemas"]["GuardrailEngineView"];
        };
        GuardrailEngineView: {
            /** @description The instance's configuration document, as stored. */
            config: Record<string, never>;
            /**
             * Format: int64
             * @description Moves whenever `config` changes. The runtime keys its compiled-engine
             *     cache by `(id, config_revision)`, so this is the number that says
             *     whether a running process is still using an older compile.
             */
            config_revision: number;
            /**
             * Format: int32
             * @description Schema version the stored `config` was written against.
             */
            config_schema_version: number;
            created_at: string;
            display_name: string;
            enabled: boolean;
            engine_key: string;
            /** Format: uuid */
            id: string;
            /**
             * Format: int64
             * @description Policies chaining this instance. Delete is refused while this is
             *     nonzero, because the foreign key is `RESTRICT`.
             */
            in_use_by_policy_count: number;
            /**
             * @description Code-owned implementation this instance runs: `secrets`, `pii`, or
             *     `term_list`. The external kinds the schema reserves appear here only
             *     once their adapters land.
             */
            kind: string;
            source: string;
            updated_at: string;
        };
        GuardrailEventWipeResponse: {
            data: components["schemas"]["GuardrailEventWipeView"];
        };
        GuardrailEventWipeView: {
            /** Format: int64 */
            audit_events: number;
            /** Format: int64 */
            finding_groups: number;
            /** Format: int64 */
            finding_reviews: number;
            /** Format: int64 */
            policy_day_states: number;
            /** Format: int64 */
            verdicts: number;
        };
        GuardrailFindingGroupListResponse: {
            data: components["schemas"]["GuardrailFindingGroupView"][];
            next_cursor?: string | null;
        };
        GuardrailFindingGroupView: {
            blocked: boolean;
            category: string;
            /**
             * Format: uuid
             * @description Engine instance that produced the group. Null for groups recorded
             *     before verdicts carried an engine id.
             */
            engine_id?: string | null;
            engine_key: string;
            engine_version: string;
            finding_key: string;
            first_seen_at: string;
            /**
             * @description True when this group's engine instance is at its per-instance group
             *     ceiling and has stopped creating new groups. A rule firing uniquely on
             *     every request looks like a busy queue; this says it is a misconfigured
             *     rule instead.
             */
            group_ceiling_reached: boolean;
            last_seen_at: string;
            latest_action: string;
            latest_finding: Record<string, never>;
            /** Format: int32 */
            latest_finding_index: number;
            latest_matched_segment?: Record<string, never> | null;
            latest_policy_name: string;
            /** Format: uuid */
            latest_request_id?: string | null;
            latest_surface: string;
            /** Format: uuid */
            latest_verdict_id: string;
            /** Format: int64 */
            occurrence_count: number;
            occurrences: Record<string, never>[];
            /** @description True when a category or preview-prefix ignore hides this group from Open. */
            pattern_ignored: boolean;
            policy_names: string[];
            preview: string;
            /** Format: int64 */
            request_count: number;
            /** Format: uuid */
            review_id?: string | null;
            review_note?: string | null;
            review_reason?: string | null;
            reviewed_at?: string | null;
            reviewed_by?: string | null;
            /**
             * @description Stable rule identity for a configured kind's finding; null for a
             *     detector kind, whose vocabulary is the gateway's own.
             */
            rule_identity?: string | null;
            status: string;
            surfaces: string[];
        };
        GuardrailFindingPatternIgnoreListResponse: {
            data: components["schemas"]["GuardrailFindingPatternIgnoreView"][];
        };
        GuardrailFindingPatternIgnoreResponse: {
            data: components["schemas"]["GuardrailFindingPatternIgnoreView"];
        };
        GuardrailFindingPatternIgnoreView: {
            /** Format: uuid */
            id: string;
            ignored_at: string;
            ignored_by?: string | null;
            note?: string | null;
            pattern: string;
            pattern_type: string;
            reason: string;
            /**
             * @description Rule identity this ignore is confined to, or null when it applies
             *     installation-wide.
             */
            scope_rule_identity?: string | null;
        };
        GuardrailFindingReviewResponse: {
            data: components["schemas"]["GuardrailFindingReviewView"];
        };
        GuardrailFindingReviewView: {
            finding_key: string;
            /** Format: uuid */
            id: string;
            note?: string | null;
            reason?: string | null;
            reviewed_at?: string | null;
            status: string;
        };
        GuardrailPolicyEngineView: {
            display_name: string;
            /** Format: uuid */
            engine_id: string;
            engine_key: string;
            /** Format: int32 */
            position: number;
        };
        GuardrailPolicyListResponse: {
            data: components["schemas"]["GuardrailPolicyView"][];
        };
        GuardrailPolicyResponse: {
            data: components["schemas"]["GuardrailPolicyView"];
        };
        GuardrailPolicyView: {
            attachments: components["schemas"]["GuardrailAttachmentView"][];
            created_at: string;
            /**
             * @description Effective mode by brokered surface. Every surface enforces the
             *     configured mode; the projection stays so a future surface that lands
             *     without refusal semantics can say so here.
             */
            effective_modes: components["schemas"]["GuardrailEffectiveModeView"][];
            enabled: boolean;
            engines: components["schemas"]["GuardrailPolicyEngineView"][];
            failure_stance: string;
            /** Format: uuid */
            id: string;
            /** @description Administrator-configured policy mode. */
            mode: string;
            name: string;
            source: string;
            template_divergence?: null | components["schemas"]["GuardrailTemplateDivergenceView"];
            /** @description Catalog entry this policy was applied from, if any. */
            template_id?: string | null;
            /**
             * Format: int32
             * @description Catalog version at the time it was applied.
             */
            template_version?: number | null;
            updated_at: string;
        };
        /**
         * @description A template this policy came from that the running catalog has since
         *     revised.
         */
        GuardrailTemplateDivergenceView: {
            /**
             * Format: int32
             * @description Version the running catalog carries, or null when this build's catalog
             *     no longer carries the entry at all.
             */
            catalog_version?: number | null;
            /** @description What diverged, in one sentence. */
            summary: string;
        };
        /** @description One engine a template composes, as the catalog listing renders it. */
        GuardrailTemplateEngineView: {
            display_name: string;
            /** @description Key this engine would take, without the apply sequence. */
            engine_key: string;
            /** @description Code-owned kind the instance runs. */
            kind: string;
            /**
             * @description What this engine catches, and what it misses. Read it before promoting
             *     a policy built from this template to enforce.
             */
            rationale: string;
        };
        GuardrailTemplateListResponse: {
            data: components["schemas"]["GuardrailTemplateView"][];
        };
        /** @description A value the template needs before it describes anything. */
        GuardrailTemplateParameterView: {
            description: string;
            label: string;
            name: string;
            /**
             * @description Applying without this is refused rather than creating a policy that
             *     matches nothing.
             */
            required: boolean;
        };
        GuardrailTemplateView: {
            category_tags: string[];
            description: string;
            engines: components["schemas"]["GuardrailTemplateEngineView"][];
            id: string;
            /**
             * @description `recommended` or `experimental`. An experimental entry is shipped for
             *     evaluation: apply it, read a week of its monitor verdicts, and take its
             *     rationale's evasion model seriously before treating it as a control.
             */
            maturity: string;
            name: string;
            parameters: components["schemas"]["GuardrailTemplateParameterView"][];
            /**
             * @description Where the template usually belongs. A suggestion; the apply request
             *     names the scope it actually uses.
             */
            suggested_scope: string;
            /**
             * Format: int32
             * @description Bumped whenever the entry's engines, config, or claims change. An
             *     applied policy records the version it came from, and a later catalog
             *     that moves this number is what a policy's divergence flag reports.
             */
            version: number;
        };
        /** @description One page of recorded verdicts. */
        GuardrailVerdictListResponse: {
            /** @description Verdicts, newest first. */
            data: components["schemas"]["GuardrailVerdictView"][];
            /** @description Pass as `cursor` for the next page, or null at the end. */
            next_cursor?: string | null;
        };
        /**
         * @description One recorded verdict in the cross-request listing.
         *
         *     `findings` carries the category, segment, and offsets of each match plus
         *     the redacted preview. New flagged verdicts also carry the complete
         *     surrounding normalized segment with every detected span redacted.
         */
        GuardrailVerdictView: {
            action: string;
            /** Format: uuid */
            attribution_team_id?: string | null;
            decision: string;
            degradation: string;
            engine_key: string;
            engine_version: string;
            /** Format: int32 */
            finding_count: number;
            /** @description Category, position, and a redacted preview. Never the full secret. */
            findings: unknown;
            /** Format: uuid */
            id: string;
            /** Format: int64 */
            latency_us: number;
            /**
             * @description Complete matched segments with every detected span replaced by its
             *     redacted preview, plus highlight offsets into the rendered text.
             */
            matched_segments: unknown;
            /** @description UTC timestamp, RFC3339 with microseconds. */
            occurred_at: string;
            /** Format: uuid */
            policy_id?: string | null;
            policy_mode: string;
            policy_name: string;
            /**
             * Format: uuid
             * @description Client-visible `x-request-id`, when the surface recorded one. Pass it
             *     to `getInferenceRequest` for the rest of the request's trace.
             */
            request_id?: string | null;
            /** Format: uuid */
            requested_model_id?: string | null;
            /** Format: int64 */
            scanned_bytes: number;
            surface: string;
            /** Format: uuid */
            user_id: string;
        };
        /**
         * @description How this installation and this row stand against the add-on's registered
         *     hosting preset.
         */
        HostingPresetView: {
            /**
             * @description True when the row is managed and already runs what install would
             *     declare. False with `pinned` true means a release moved the pin, or
             *     the row was declared by hand, and install updates the workload.
             */
            image_current: boolean;
            /** @description The digest this deployment pins, for a `first_party` preset with a pin. */
            image_digest?: string | null;
            /**
             * @description The image repository install runs from: the pin's, or the one the
             *     tracked releases publish to.
             */
            image_reference?: string | null;
            /**
             * @description True when install can run: a pinned image exists, or the preset
             *     tracks releases.
             */
            pinned: boolean;
            /** @description The GitHub repository a `track_release` preset follows. */
            release_repository?: string | null;
            /**
             * @description How install sources the image: `first_party` follows the image each
             *     gateway release pins for this add-on (`add_ons.hosting.first_party`);
             *     `track_release` follows the add-on's own GitHub releases (ADR 0108).
             */
            source: string;
            variables: components["schemas"]["PresetVariableView"][];
        };
        /** @description Identity provider listing. */
        IdentityProviderListResponse: {
            /** @description Every provider, whichever authority owns it. */
            data: components["schemas"]["IdentityProviderView"][];
        };
        /**
         * @description Storage authority for an identity-provider configuration.
         * @enum {string}
         */
        IdentityProviderSource: "config" | "database";
        /**
         * @description Identity-provider verification state.
         * @enum {string}
         */
        IdentityProviderStatus: "draft" | "verified" | "error";
        /** @description A federated sign-in provider, without its client secret. */
        IdentityProviderView: {
            /** @description Scopes requested beyond the built-in OIDC ones. */
            additional_scopes: string[];
            /** @description Email domains allowed to create accounts; empty means unrestricted. */
            allowed_email_domains: string[];
            /** @description OIDC client identifier. */
            client_id: string;
            /** @description Whether the provider is offered at sign-in. */
            enabled: boolean;
            /** @description Claim read for group-to-role mapping. */
            groups_claim: string;
            /**
             * Format: uuid
             * @description Stable provider ID.
             */
            id: string;
            /** @description Exact OIDC issuer. */
            issuer: string;
            /** @description Whether an unmatched verified identity may create an account at sign-in. */
            jit_enabled: boolean;
            /** @description Administrator-facing name, shown on the sign-in page. */
            name: string;
            /** @description Whether client-secret material is stored. */
            secret_configured: boolean;
            /** @description Whether deployment configuration or this API owns the provider. */
            source: components["schemas"]["IdentityProviderSource"];
            /** @description Whether a verification round trip has succeeded. */
            status: components["schemas"]["IdentityProviderStatus"];
            /** @description When verification last succeeded. */
            verified_at?: string | null;
        };
        IgnoreGuardrailFindingPatternRequest: {
            note?: string | null;
            pattern: string;
            pattern_type: components["schemas"]["FindingPatternType"];
            reason: components["schemas"]["FindingReviewReason"];
            /**
             * @description Confine the ignore to one rule identity. Omitted, the ignore is
             *     installation-wide, which is what a category ignore usually wants and
             *     what every ignore was before ADR 0066.
             */
            scope_rule_identity?: string | null;
        };
        /**
         * @description Who built the digest a managed row runs, as a verified attestation
         *     established (ADR 0108).
         */
        ImageProvenanceView: {
            /**
             * @description The signing certificate's identity: the workflow URL and the ref the
             *     run used.
             */
            signer_identity: string;
            /** @description The GitHub repository the policy required, as `owner/name`. */
            source_repository: string;
            /** @description When this gateway verified the attestation. */
            verified_at: string;
            /** @description The workflow file the policy required. */
            workflow: string;
        };
        /** @description Who receives access to every model the import creates. */
        ImportGrantsRequest: {
            /** @description Teams granted each created model. */
            team_ids?: string[];
            /** @description People granted each created model directly, independent of any team. */
            user_ids?: string[];
        };
        /** @description Why one import item was skipped. */
        ImportItemError: {
            /**
             * @description Stable machine-readable rule identifier, the same codes
             *     `createProviderModel` rejects with.
             */
            code: string;
            /** @description Request field the rule applies to. */
            field: string;
            /** @description Human-readable explanation of what to change. */
            message: string;
        };
        /** @description Outcome of one import item. */
        ImportItemResult: {
            error?: null | components["schemas"]["ImportItemError"];
            /**
             * @description The item's gateway ID as submitted (trimmed), so results can be
             *     correlated without relying on order alone.
             */
            gateway_id: string;
            model?: null | components["schemas"]["ModelView"];
        };
        /**
         * @description One model inside an import request.
         *
         *     The fields mirror [`CreateModelRequest`] minus `provider_id`, which the
         *     import names once for the whole batch. The same validation rules apply per
         *     item, and a rejection carries the same stable codes.
         */
        ImportModelItem: {
            /** @description One-hour cache-write rate. */
            cache_write_1h_price_per_million?: string | null;
            /** @description Five-minute cache-write rate. */
            cache_write_5m_price_per_million?: string | null;
            /** @description Cache-read rate. Omit to bill cached input at the input rate. */
            cached_input_price_per_million?: string | null;
            /** @description Canonical model identity; omit to default to the gateway ID. */
            canonical_model_id?: string | null;
            /**
             * Format: int32
             * @description Context window in tokens. Leave null when the publisher has not stated
             *     one; never estimate.
             */
            context_window?: number | null;
            /** @description Administrator-facing name. */
            display_name: string;
            /** @description Fast-tier one-hour cache-write rate. */
            fast_cache_write_1h_price_per_million?: string | null;
            /** @description Fast-tier five-minute cache-write rate. */
            fast_cache_write_5m_price_per_million?: string | null;
            /** @description Fast-tier cache-read rate. Omit to bill fast cached input at the fast input rate. */
            fast_cached_input_price_per_million?: string | null;
            /**
             * @description Premium-tier input rate, covering Anthropic fast mode and `OpenAI` priority
             *     processing alike. Fast input and output are all-or-nothing, and a fast cache rate
             *     requires a fast input rate: the fast rates never fall back to the standard ones,
             *     because a premium tier costs more and reading across understates it.
             */
            fast_input_price_per_million?: string | null;
            /** @description Fast-tier output rate in US dollars per million tokens. */
            fast_output_price_per_million?: string | null;
            /** @description Flex-tier one-hour cache-write rate. */
            flex_cache_write_1h_price_per_million?: string | null;
            /** @description Flex-tier five-minute cache-write rate. */
            flex_cache_write_5m_price_per_million?: string | null;
            /** @description Flex-tier cache-read rate. Omit to bill flex cached input at the flex input rate. */
            flex_cached_input_price_per_million?: string | null;
            /**
             * @description Discount-tier input rate, covering `OpenAI` flex processing. Flex input and output
             *     are all-or-nothing, and a flex cache rate requires a flex input rate: the flex
             *     rates never fall back to the standard ones, because a discount tier costs less and
             *     reading across overstates it.
             */
            flex_input_price_per_million?: string | null;
            /** @description Flex-tier output rate in US dollars per million tokens. */
            flex_output_price_per_million?: string | null;
            /** @description Identifier clients will request. Lowercase letters, digits, and `._:-`. */
            gateway_id: string;
            /**
             * @description Input rate in US dollars per million tokens. Input and output rates are
             *     all-or-nothing; any cache rate requires an input rate.
             */
            input_price_per_million?: string | null;
            /**
             * Format: int32
             * @description Input token count above which estimates apply the long-context surcharge.
             */
            long_context_input_threshold?: number | null;
            /**
             * Format: int32
             * @description Maximum output tokens per response. Leave null when unstated.
             */
            max_output_tokens?: number | null;
            /** @description Output rate in US dollars per million tokens. */
            output_price_per_million?: string | null;
            /**
             * @description How this route binds requests to provider-provisioned capacity.
             *     Defaults to `none`.
             */
            provisioned_capacity_mode?: components["schemas"]["ProvisionedCapacityMode"];
            /**
             * @description Reasoning effort tokens this model's upstream accepts. Omit to leave
             *     the ladder unstated; an empty array records no effort control.
             */
            supported_reasoning_efforts?: components["schemas"]["ReasoningEffort"][] | null;
            /** @description Whether to expose the model on the Anthropic Message Batches surface. */
            supports_anthropic_message_batches?: boolean;
            /** @description Whether to expose the model on the Anthropic Messages surface. */
            supports_anthropic_messages?: boolean;
            /** @description Whether to expose the model on the `OpenAI` Responses surface. */
            supports_openai_responses?: boolean;
            /** @description Whether the model supports tool calls. */
            supports_tools?: boolean;
            /** @description Whether the model accepts image input. */
            supports_vision?: boolean;
            /** @description Ultrafast-tier one-hour cache-write rate. */
            ultrafast_cache_write_1h_price_per_million?: string | null;
            /** @description Ultrafast-tier five-minute cache-write rate. */
            ultrafast_cache_write_5m_price_per_million?: string | null;
            /** @description Ultrafast-tier cache-read rate. Omit to use the Ultrafast input rate. */
            ultrafast_cached_input_price_per_million?: string | null;
            /**
             * @description Contract input rate for `OpenAI` Ultrafast processing. Ultrafast input and output
             *     are all-or-nothing, and an Ultrafast cache rate requires an Ultrafast input rate.
             *     These rates never fall back to Fast or Standard.
             */
            ultrafast_input_price_per_million?: string | null;
            /** @description Ultrafast-tier output rate in US dollars per million tokens. */
            ultrafast_output_price_per_million?: string | null;
            /** @description Exact identifier the upstream expects. */
            upstream_id: string;
        };
        /** @description Request to add several models under one provider in a single operation. */
        ImportModelsRequest: {
            /**
             * @description Whether created models may route traffic immediately. Defaults to true:
             *     an import that also grants access is asking for reachable models, and a
             *     disabled-by-default batch would reintroduce the forgotten-enable step
             *     this operation exists to remove.
             */
            enabled?: boolean;
            /**
             * @description Access granted to each created model. An import with empty grants is
             *     accepted — staging is legitimate — but the models it creates are
             *     unreachable until granted, and the listing flags them as such.
             */
            grants?: components["schemas"]["ImportGrantsRequest"];
            /** @description Models to create. At least one, at most 100 per request. */
            models: components["schemas"]["ImportModelItem"][];
            /**
             * Format: uuid
             * @description Provider that will serve every model in the batch.
             */
            provider_id: string;
        };
        /** @description Import outcome, item by item. */
        ImportModelsResponse: {
            /** @description How many items were created. */
            created: number;
            /** @description One result per submitted model, in submission order. */
            data: components["schemas"]["ImportItemResult"][];
            /** @description How many items were rejected. */
            rejected: number;
        };
        /** @description One inference event recorded under the traced request ID. */
        InferenceRequestEventView: {
            /**
             * @description Which attempt of a failover waterfall this row was: `primary`,
             *     `failover`, or `abandoned`. Null means unrecorded — the event predates
             *     the vocabulary — and is never readable as a committed attempt.
             *
             *     The field this trace exists for. One `x-request-id` covers every
             *     attempt the gateway made for one northbound request, so a trace that
             *     omitted this would leave the committed answer indistinguishable from
             *     the held responses the waterfall walked past.
             * @example failover
             */
            attempt_role?: string | null;
            /**
             * @description Client-supplied item identifier within the Message Batch.
             * @example job-001
             */
            batch_custom_id?: string | null;
            /**
             * @description Gateway-owned Message Batch identifier for an asynchronous item.
             * @example msgbatch_01k39f4fb5x7x0q2a1m6s9v8te
             */
            batch_id?: string | null;
            /** @description How the serving capacity was paid for. */
            billing_class: components["schemas"]["BillingClass"];
            /**
             * Format: int64
             * @description Known cache-write input tokens.
             */
            cache_write_input_tokens?: number | null;
            /**
             * Format: int64
             * @description Known cached (cache-read) input tokens.
             */
            cached_input_tokens?: number | null;
            /**
             * @description Client that minted the access token, or null for rows that predate
             *     client attribution.
             * @example opencode
             */
            client_name?: string | null;
            /**
             * Format: int32
             * @description Catalog context window of the route that served this attempt. Null when
             *     the catalog recorded none. Read against `input_tokens` it is what says
             *     whether the attempt was near the window it was routed to.
             */
            context_window?: number | null;
            /**
             * Format: uuid
             * @description Conversation this event was attributed to, or null when the client
             *     sent no attribution. When present, `getConversation` reads the
             *     surrounding turns.
             */
            conversation_id?: string | null;
            /**
             * @description `priced`, `pending_rate`, `provisional`, or `unpriceable`.
             * @example priced
             */
            cost_state: string;
            /**
             * @description Credential class that produced this attempt: `user_subscription`,
             *     `provider_api_key`, `workload_identity`, or `none`.
             * @example user_subscription
             */
            credential_class: string;
            /**
             * @description Why resolution chose this credential (ADR 0027): `subscription_used`,
             *     `subscription_shared_used`, `subscription_fallback`,
             *     `subscription_cooling_down`, `subscription_reauth_required`,
             *     `subscription_share_cap_reached`, and the provider-level gates. Null
             *     when the provider has no subscription dimension or the row predates
             *     the vocabulary. This is the field that says which account served a
             *     request and, when none did, why not.
             * @example subscription_reauth_required
             */
            credential_resolution_reason?: string | null;
            /**
             * Format: int64
             * @description Wall-clock request duration in milliseconds.
             */
            duration_ms: number;
            /** @description Stable content-free failure code, when one was recorded. */
            error_code?: string | null;
            /**
             * Format: int64
             * @description Estimated request cost in micro-US-dollars.
             */
            estimated_cost_microusd?: number | null;
            /**
             * @description True when a subscription was attempted first and a metered credential
             *     served instead.
             */
            fallback_from_subscription: boolean;
            /**
             * @description The provider's own finish or stop reason, raw and unnormalized.
             * @example stop
             */
            finish_reason?: string | null;
            /**
             * Format: int64
             * @description Time-to-first-byte in milliseconds, when an upstream response was observed.
             */
            first_byte_ms?: number | null;
            /**
             * @description Client-visible model ID, as recorded on the event itself rather than
             *     read from the live catalog, which may have since renamed or deleted
             *     this model.
             * @example kimi-k3
             */
            gateway_model_id: string;
            /**
             * Format: uuid
             * @description Usage event ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens?: number | null;
            /**
             * Format: int32
             * @description Northbound turn items presented to the model. Null means uncounted.
             */
            message_count?: number | null;
            /**
             * Format: uuid
             * @description Catalog model, or null when the event predates model attribution or its
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /**
             * @description UTC event timestamp, RFC3339.
             * @example 2026-08-06T14:30:08Z
             */
            occurred_at: string;
            /**
             * @description Terminal request outcome: `succeeded`, `denied`, or `failed`.
             * @example succeeded
             */
            outcome: string;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens?: number | null;
            /**
             * @description Northbound protocol the request arrived over: `anthropic_messages` or
             *     `openai_responses`.
             * @example openai_responses
             */
            protocol: string;
            /** @description Provider display name, likewise recorded on the event. */
            provider_name: string;
            provisioned_capacity_evidence?: null | components["schemas"]["ProvisionedCapacityEvidence"];
            /**
             * Format: int64
             * @description Known reasoning output tokens.
             */
            reasoning_output_tokens?: number | null;
            /** @description `user_request`, `background_model_task`, or `message_batch`. */
            source_kind: string;
            /**
             * Format: uuid
             * @description The subscription binding this attempt drew on, or fell back from when
             *     `fallback_from_subscription` is true. Null when no subscription was
             *     involved.
             */
            subscription_binding_id?: string | null;
            /**
             * @description That binding's label, as shown on the account page. Null when the
             *     binding has since been deleted or none was involved.
             * @example thet@brightwave.io
             */
            subscription_label?: string | null;
            /**
             * @description Email of the account that owns the binding. Under team sharing this is
             *     the teammate whose quota was drawn down, not the requester (ADR 0037).
             */
            subscription_owner_email?: string | null;
            /**
             * @description How the upstream response ended: `completed`, `incomplete`,
             *     `truncated`, or `failed`. Null is the unrecorded group, not a state:
             *     the event predates this vocabulary or was served by a path that
             *     reports no terminal state. It is never a completed response.
             * @example completed
             */
            terminal_state?: string | null;
            /**
             * Format: int32
             * @description Tool or function call items the response carried. Null means
             *     uncounted, which is distinct from a counted zero.
             */
            tool_call_count?: number | null;
            /**
             * Format: int32
             * @description Declared tool/function definitions on the request. Null means
             *     uncounted, which is distinct from a counted zero.
             */
            tool_definition_count?: number | null;
            /**
             * Format: int32
             * @description HTTP status the upstream answered with, when one was reached.
             */
            upstream_status?: number | null;
            /**
             * Format: uuid
             * @description The account the request was served for.
             */
            user_id: string;
        };
        /** @description Every visible inference event recorded under one request ID. */
        InferenceRequestView: {
            /**
             * @description Matching events, newest first. Empty when the ID was never recorded —
             *     or belongs to traffic the caller may not see; the two are deliberately
             *     indistinguishable.
             */
            events: components["schemas"]["InferenceRequestEventView"][];
            /** @description True when more events carry this ID than the response returns. */
            events_truncated: boolean;
            /**
             * @description Guardrail verdicts recorded under this request ID (ADR 0055). Empty
             *     when none ran or the caller may not see them. New flagged verdicts
             *     include their full surrounding matched segments with secrets redacted.
             */
            guardrail_verdicts: components["schemas"]["GuardrailVerdictView"][];
            /**
             * Format: uuid
             * @description The traced request ID, echoed back.
             */
            request_id: string;
        };
        /** @enum {string} */
        IssueCategory: "system" | "pricing" | "usage" | "cost" | "sandbox";
        IssueCategorySummaryView: {
            category: components["schemas"]["IssueCategory"];
            group_count: number;
            /** Format: int64 */
            occurrence_count: number;
        };
        /** @enum {string} */
        IssueDisposition: "actionable" | "expected" | "unavoidable";
        IssueGroupView: {
            /** @description Broad console and filtering category. */
            category: components["schemas"]["IssueCategory"];
            /**
             * @description Stable machine-readable cause.
             * @example missing_model_rates
             */
            cause: string;
            /** @description Why the group received its disposition. */
            description: string;
            /** @description Whether an administrator hid this stable cause from active issue reads. */
            dismissed: boolean;
            /**
             * @description Whether an operator should fix this group or only retain it as history.
             *     Expected causes are classified and then omitted from the response.
             */
            disposition: components["schemas"]["IssueDisposition"];
            /** @description Bounded structured facts and excerpts that support the diagnosis. */
            evidence: unknown;
            /** @description First occurrence in the selected window. */
            first_seen_at: string;
            /**
             * @description Stable content-derived key for this deduplicated cause.
             * @example pricing:pending_rate:78f22b6a6c42a35f
             */
            key: string;
            /**
             * @description Stable issue kind within the category.
             * @example pending_rate
             */
            kind: string;
            /** @description Most recent occurrence in the selected window. */
            last_seen_at: string;
            /** @description Copyable identifiers from the latest exemplar. */
            latest: components["schemas"]["IssueIdentifiersView"];
            /**
             * Format: int64
             * @description Events or tasks represented by this group.
             */
            occurrence_count: number;
            route?: null | components["schemas"]["IssueRouteView"];
            /** @description Visual and ordering priority. */
            severity: components["schemas"]["IssueSeverity"];
            /** @description The next investigation or configuration change to make. */
            suggested_action: string;
            /** @description Short operator-facing title. */
            title: string;
        };
        IssueIdentifiersView: {
            /**
             * Format: uuid
             * @description Attempt family attached to the exemplar, when recorded.
             */
            attempt_family_id?: string | null;
            /**
             * Format: uuid
             * @description Conversation attached to the exemplar, when recorded.
             */
            conversation_id?: string | null;
            /**
             * Format: uuid
             * @description Most recent inference event in this group.
             */
            inference_event_id?: string | null;
            /**
             * Format: uuid
             * @description Northbound request ID on the exemplar, when recorded.
             */
            request_id?: string | null;
            /**
             * Format: uuid
             * @description Most recent platform-owned task in this group.
             */
            task_id?: string | null;
        };
        /** @enum {string} */
        IssueRouteStatus: "active" | "disabled" | "removed" | "unconfigured";
        IssueRouteView: {
            /** @description Client-visible model snapshot. */
            gateway_id?: string | null;
            /**
             * Format: uuid
             * @description Stable catalog model, when the row still exists or the event retained it.
             */
            model_id?: string | null;
            /**
             * Format: uuid
             * @description Stable provider, when retained.
             */
            provider_id?: string | null;
            /** @description Provider display-name snapshot. */
            provider_name?: string | null;
            /** @description Whether this route can still serve requests. */
            status: components["schemas"]["IssueRouteStatus"];
        };
        /** @enum {string} */
        IssueSeverity: "error" | "warning" | "info";
        IssuesResponse: {
            /** @description Deduplicated groups, actionable first and then newest first. */
            groups: components["schemas"]["IssueGroupView"][];
            /** @description Inclusive RFC3339 lower bound applied, or null for all history. */
            since?: string | null;
            summary: components["schemas"]["IssuesSummaryView"];
            /** @description True when more groups matched than this response can carry. */
            truncated: boolean;
            /** @description Exclusive RFC3339 upper bound applied, or null for no upper bound. */
            until?: string | null;
        };
        IssuesSummaryView: {
            actionable_groups: number;
            by_category: components["schemas"]["IssueCategorySummaryView"][];
            group_count: number;
            /** Format: int64 */
            occurrence_count: number;
            unavoidable_groups: number;
        };
        /**
         * @description Installation-wide interactive sign-in policy.
         * @enum {string}
         */
        LoginMode: "local_only" | "local_or_sso" | "sso_required";
        MachineBindingListResponse: {
            data: components["schemas"]["MachineBindingView"][];
        };
        MachineBindingRequest: {
            canonical_public_url: string;
        };
        MachineBindingView: {
            canonical_public_url: string;
            derived_digest: string;
        };
        /** @description One connected app and the exact operations pinned on it. */
        ManifestBinding: {
            /**
             * Format: uuid
             * @description Connected app the shared app may call, as the viewer.
             */
            connected_app_id: string;
            /** @description Operation ids drawn from that app's ingested catalog. */
            operation_ids: string[];
            /** @description Reserved for per-operation policy. Every key is refused in v1. */
            operation_policies?: Record<string, never> | null;
        };
        /**
         * @description One input the app takes.
         *
         *     Kept loose on purpose: the shell renders it and the bundle reads it, and
         *     neither is a security boundary. Bounding the count and the strings is enough
         *     — a vocabulary fixed now would have to be widened by a migration the first
         *     time an author wants a date picker.
         */
        ManifestParameter: {
            /** @description Free-form kind hint for the shell, such as `string`. */
            kind: string;
            /** @description Name the bundle reads the value under. */
            name: string;
        };
        /** @description MCP endpoint listing. */
        McpEndpointListResponse: {
            /** @description Every endpoint, deleted ones excluded, both owner kinds included. */
            data: components["schemas"]["McpEndpointView"][];
        };
        /** @description An MCP endpoint and the terms clients reach it on. */
        McpEndpointView: {
            /** @description The connected apps this endpoint exposes as tools. */
            app_ids: string[];
            /** @description Whether clients may connect. */
            enabled: boolean;
            /** @description Whether any authorized client may invoke, or only attested tool calls. */
            execution_mode: components["schemas"]["McpExecutionMode"];
            /**
             * Format: uuid
             * @description Stable endpoint ID.
             */
            id: string;
            /** @description Administrator-facing name. */
            name: string;
            /** @description Whether the installation or one user owns the endpoint. */
            owner_type: components["schemas"]["McpOwnerType"];
            /**
             * Format: uuid
             * @description The owning user, for a user-owned endpoint.
             */
            owner_user_id?: string | null;
            /** @description Slug the endpoint answers on, under `/mcp/{slug}`. */
            slug: string;
        };
        /**
         * @description Authorization mode for aggregate MCP tool execution.
         * @enum {string}
         */
        McpExecutionMode: "direct" | "gateway_attested";
        /**
         * @description Principal that owns an aggregate MCP endpoint.
         * @enum {string}
         */
        McpOwnerType: "installation" | "user";
        MintedWebhookSecretResponse: {
            /**
             * @description The minted webhook secret, shown exactly once: the row keeps only its
             *     digest, the plaintext lives in the workload's webhook Secret and the
             *     vendor's webhook configuration, and no path can recover it.
             */
            webhook_secret: string;
        };
        /** @enum {string} */
        Mode: "monitor" | "enforce";
        /**
         * @description Model-route policy for invoking a connected app.
         * @enum {string}
         */
        ModelAccessMode: "all" | "allowlist";
        /** @description Who holds one model: the granted teams, and the accounts they reach. */
        ModelAccessResponse: {
            /** @description Teams whose grant reaches the model, in name order. */
            teams: components["schemas"]["ModelTeamGrantView"][];
            /** @description Everyone a grant reaches, folded to one row each, in name order. */
            users: components["schemas"]["ModelUserView"][];
        };
        /** @description Event count for one `cost_state`, scoped to one model. */
        ModelCostStateCountView: {
            /**
             * @description `priced`, `pending_rate`, `provisional`, or `unpriceable`.
             * @example pending_rate
             */
            cost_state: string;
            /**
             * Format: int64
             * @description Number of inference events carrying that classification.
             */
            event_count: number;
            /**
             * @description Client-visible model ID, as recorded on the events themselves rather
             *     than read from the live catalog, which may have since renamed or
             *     deleted this model.
             * @example gpt-5.6-sol
             */
            gateway_id: string;
            /**
             * Format: uuid
             * @description Catalog model, or null when the event predates model attribution or its
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /** @description Provider display name, likewise recorded on the events. */
            provider_name: string;
        };
        /**
         * @description Event counts for one raw upstream finish reason, scoped to one model.
         *
         *     This is the audit trail for the terminal-state mapping: the provider's own
         *     terminal string, verbatim, beside the state the gateway classified it as.
         *     A reason the vocabulary never anticipated maps to `completed` rather than
         *     being refused — and this breakdown is where that reason surfaces as its own
         *     row instead of dissolving into the count.
         */
        ModelFinishReasonCountView: {
            /**
             * Format: int64
             * @description Number of inference events in this group.
             */
            event_count: number;
            /**
             * @description The provider's terminal string, recorded verbatim — `stop`,
             *     `tool_calls`, `length`, `end_turn`, or any value the upstream chose to
             *     send. Null is the unrecorded group, not an observed value: those events
             *     predate the vocabulary or were served by a path that reports no reason.
             * @example stop
             */
            finish_reason?: string | null;
            /**
             * @description Client-visible model ID, as recorded on the events themselves rather
             *     than read from the live catalog, which may have since renamed or
             *     deleted this model.
             * @example kimi-k3
             */
            gateway_id: string;
            /**
             * Format: uuid
             * @description Catalog model, or null when the event predates model attribution or its
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /** @description Provider display name, likewise recorded on the events. */
            provider_name: string;
            /**
             * @description How the gateway classified responses carrying this reason:
             *     `completed`, `incomplete`, `truncated`, or `failed`. Null is the
             *     unrecorded group, not a state — never a completed response.
             * @example completed
             */
            terminal_state?: string | null;
            /**
             * Format: int64
             * @description Events in this group that carried a counted zero tool calls. Zero for
             *     the null-reason group by definition: an event that recorded no reason
             *     answers neither way.
             */
            text_only_count: number;
        };
        /** @description Catalog listing. */
        ModelListResponse: {
            /** @description Every catalog model, ordered by provider then display name. */
            data: components["schemas"]["ModelView"][];
        };
        /**
         * @description Credential mechanism used by a model provider.
         * @enum {string}
         */
        ModelProviderAuthMethod: "workload_identity" | "api_key" | "none";
        /**
         * @description Supported model-provider adapter.
         * @enum {string}
         */
        ModelProviderKind: "bedrock" | "anthropic" | "openai" | "google_vertex" | "xai" | "opencode_go" | "openai_compatible" | "development_fixture";
        /**
         * @description One team whose grant reaches a model.
         *
         *     Shared with the versioned API rather than restated there, so the console and
         *     an agent cannot disagree about what "this team holds the model" means.
         */
        ModelTeamGrantView: {
            /**
             * @description Whether the team currently confers inherited access. A disabled team
             *     keeps its grant and stops conferring it, so a listed team is not by
             *     itself proof that anyone can route here.
             */
            enabled: boolean;
            /**
             * @description Display name of the administrator who wrote the grant; null once that
             *     account is deleted, or when the grant predates the record.
             */
            granted_by?: string | null;
            /**
             * Format: uuid
             * @description Stable team ID.
             */
            id: string;
            /** @description Administrator-facing name. */
            name: string;
            /** @description Stable URL slug. */
            slug: string;
        };
        /** @description Event counts for one `terminal_state`, scoped to one model. */
        ModelTerminalStateCountView: {
            /**
             * Format: int64
             * @description Number of inference events in this group.
             */
            event_count: number;
            /**
             * @description Client-visible model ID, as recorded on the events themselves rather
             *     than read from the live catalog, which may have since renamed or
             *     deleted this model.
             * @example kimi-k3
             */
            gateway_id: string;
            /**
             * Format: uuid
             * @description Catalog model, or null when the event predates model attribution or its
             *     catalog row has since been deleted.
             */
            model_id?: string | null;
            /** @description Provider display name, likewise recorded on the events. */
            provider_name: string;
            /**
             * @description `completed`, `incomplete`, `truncated`, or `failed`. Null is the
             *     unrecorded group, not a state: those events predate this vocabulary or
             *     were served by a path that reports no terminal state. It is never a
             *     completed response.
             * @example truncated
             */
            terminal_state?: string | null;
            /**
             * Format: int64
             * @description Events in this group that reported a finish reason and carried a counted
             *     zero tool calls — a model that answered in prose where a call was
             *     expected. Deliberately narrower than "no tool calls": an event that
             *     never counted its calls is unrecorded rather than zero, and cannot
             *     answer this question either way.
             */
            text_only_count: number;
        };
        /** @description Traffic for one catalog model, as recorded at request time. */
        ModelUsageView: {
            /**
             * Format: int64
             * @description Abandoned failover attempts.
             */
            abandoned_requests: number;
            /**
             * Format: int64
             * @description Mean committed-request duration in milliseconds.
             */
            avg_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Mean committed-request time-to-first-byte in milliseconds.
             */
            avg_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Mean northbound message count across committed requests that recorded one.
             */
            avg_message_count?: number | null;
            /**
             * Format: double
             * @description Mean declared tool-definition count across committed requests that recorded one.
             */
            avg_tool_definition_count?: number | null;
            /**
             * Format: double
             * @description Mean user-message count across committed requests that recorded one.
             */
            avg_user_message_count?: number | null;
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Cached (cache-read) input tokens on committed requests.
             */
            cached_input_tokens: number;
            /**
             * @description Underlying model the gateway ID served, independent of the route, as
             *     resolved when the requests ran. Rows sharing this value are the same
             *     model on different providers. Null when neither the events nor the
             *     catalog record one.
             */
            canonical_model_id?: string | null;
            /**
             * Format: int64
             * @description Committed requests (`attempt_role` is not abandoned).
             */
            committed_requests: number;
            /**
             * Format: int64
             * @description Prepaid usage-credits draw-down, in micro-US-dollars.
             */
            credits_cost_microusd: number;
            /**
             * Format: int64
             * @description Estimated metered (installation-billed) cost, in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Cache-write input tokens on committed first-turn requests.
             */
            first_turn_cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Estimated billed cost of committed first-turn requests, in
             *     micro-US-dollars.
             */
            first_turn_cost_microusd: number;
            /**
             * Format: int64
             * @description Input tokens on committed first-turn requests.
             */
            first_turn_input_tokens: number;
            /**
             * Format: int64
             * @description Committed first-turn requests: one user message and nothing else in
             *     the transcript beyond the standing instructions block — the opening
             *     request of a conversation, never a tool-loop continuation. What
             *     starting a session costs — system prompt, standing instruction files,
             *     and tool definitions land here with nothing yet amortized.
             */
            first_turn_requests: number;
            /** @description Identifier clients requested. */
            gateway_model_id: string;
            /**
             * Format: int64
             * @description Requests served, including abandoned failovers.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Input tokens across committed requests.
             */
            input_tokens: number;
            /**
             * Format: int64
             * @description Output tokens across committed requests.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Median committed-request duration in milliseconds.
             */
            p50_duration_ms?: number | null;
            /**
             * Format: int64
             * @description Median committed-request time-to-first-byte in milliseconds.
             */
            p50_first_byte_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request duration in milliseconds.
             */
            p95_duration_ms?: number | null;
            /**
             * Format: int64
             * @description 95th-percentile committed-request time-to-first-byte in milliseconds.
             */
            p95_first_byte_ms?: number | null;
            /**
             * Format: double
             * @description Peak input-tokens / context-window across committed requests.
             */
            peak_context_utilization?: number | null;
            /** Format: int64 */
            pending_rate_requests: number;
            /**
             * Format: int64
             * @description How many of those requests carry a final price.
             */
            priced_requests: number;
            /**
             * Format: uuid
             * @description Provider that served the requests.
             */
            provider_id: string;
            /** @description Provider kind recorded with the requests. */
            provider_kind: components["schemas"]["ModelProviderKind"];
            /** @description Provider name recorded with the requests. */
            provider_name: string;
            /** Format: int64 */
            provisional_requests: number;
            /**
             * Format: int64
             * @description Cost absorbed by provider-provisioned capacity, in micro-US-dollars.
             */
            provisioned_cost_microusd: number;
            /**
             * Format: int64
             * @description Committed requests that recorded request-shape counts — the denominator
             *     for the first-turn statistics. History that predates shape capture has
             *     no `user_message_count` and is excluded from them, not counted as zero.
             */
            shape_recorded_requests: number;
            /**
             * Format: int64
             * @description Notional cost absorbed by user subscriptions, in micro-US-dollars.
             */
            subscription_cost_microusd: number;
            /**
             * Format: int64
             * @description Requests that rode a user subscription credential — includes
             *     credit-served traffic, matching the by-user rollup contract.
             */
            subscription_requests: number;
            /** Format: int64 */
            unpriceable_requests: number;
            /** @description Identifier sent upstream. */
            upstream_model_id: string;
        };
        /** @description One granting team, as a person's row names it. */
        ModelUserTeamView: {
            /**
             * Format: uuid
             * @description Stable team ID.
             */
            id: string;
            /** @description Administrator-facing name. */
            name: string;
        };
        /** @description One account a grant on the model reaches, and how it reaches them. */
        ModelUserView: {
            /** @description Whether the account can still sign in. */
            active: boolean;
            /**
             * @description Whether this account holds the model directly, beside whatever its
             *     teams confer. A direct grant survives losing every team.
             */
            direct_grant: boolean;
            /** @description Email, empty when the account carries none. */
            email: string;
            /**
             * @description Display name of the administrator who wrote the direct grant; null when
             *     there is none, once that account is deleted, or when the grant predates
             *     the record. Who wrote a team's grant is on that team's own row.
             */
            granted_by?: string | null;
            /**
             * @description Teams whose grant confers the access, in name order. Empty when only a
             *     direct grant reaches this account.
             */
            granted_via: components["schemas"]["ModelUserTeamView"][];
            /**
             * Format: uuid
             * @description The account ID: a person's, or a service identity's.
             */
            id: string;
            /** @description Display name, falling back to email then username. */
            name: string;
            /** @description Whether the account is a service identity rather than a person. */
            service_identity: boolean;
        };
        /** @description A model in the catalog.// Capability flags are a flat set on the wire because that is how the catalog */
        ModelView: {
            /** @description One-hour cache-write rate, or null to leave those writes unpriced. */
            cache_write_1h_price_per_million?: string | null;
            /** @description Five-minute cache-write rate, or null to leave those writes unpriced. */
            cache_write_5m_price_per_million?: string | null;
            /** @description Cache-read rate, or null to bill cached input at the input rate. */
            cached_input_price_per_million?: string | null;
            /**
             * @description Which underlying model this row serves, independent of the serving
             *     provider — the same value on two models says they answer with the same
             *     model. Null only for rows written mid-rollout by a pre-upgrade release.
             * @example gpt-5.6-sol
             */
            canonical_model_id?: string | null;
            /**
             * @description True when another enabled, picker-visible Anthropic model has the same
             *     normalized display name and would be indistinguishable when co-granted.
             */
            claude_code_picker_name_conflict: boolean;
            /**
             * Format: int32
             * @description Context window in tokens, or null when the publisher has not stated one.
             */
            context_window?: number | null;
            /** @description Administrator-facing name. */
            display_name: string;
            /** @description Whether users may be routed here. */
            enabled: boolean;
            /** @description Fast-tier one-hour cache-write rate, or null. */
            fast_cache_write_1h_price_per_million?: string | null;
            /** @description Fast-tier five-minute cache-write rate, or null. */
            fast_cache_write_5m_price_per_million?: string | null;
            /** @description Fast-tier cache-read rate, or null to bill fast cached input at the fast rate. */
            fast_cached_input_price_per_million?: string | null;
            /**
             * @description Fast-tier input rate, or null when the model has no fast rate configured.
             *
             *     Null does not mean fast requests are free or standard-priced: a fast-served
             *     request against a model with no fast rate is recorded `pending_rate` and stays
             *     unpriced until this is set.
             */
            fast_input_price_per_million?: string | null;
            /** @description Fast-tier output rate, or null. */
            fast_output_price_per_million?: string | null;
            /** @description Flex-tier one-hour cache-write rate, or null. */
            flex_cache_write_1h_price_per_million?: string | null;
            /** @description Flex-tier five-minute cache-write rate, or null. */
            flex_cache_write_5m_price_per_million?: string | null;
            /** @description Flex-tier cache-read rate, or null to bill flex cached input at the flex rate. */
            flex_cached_input_price_per_million?: string | null;
            /**
             * @description Flex-tier input rate, or null when the model has no flex rate configured.
             *
             *     Null does not mean flex requests are billed at the standard rate: a flex-served
             *     request against a model with no flex rate is recorded `pending_rate` and stays
             *     unpriced until this is set.
             */
            flex_input_price_per_million?: string | null;
            /** @description Flex-tier output rate, or null. */
            flex_output_price_per_million?: string | null;
            /**
             * @description Identifier clients request this model by.
             * @example gpt-5.6-sol
             */
            gateway_id: string;
            /**
             * @description True when Claude Code's gateway model discovery will not list this
             *     model: it serves Anthropic Messages but its `gateway_id` starts with
             *     neither `claude` nor `anthropic`, and discovery silently drops such
             *     ids from the /model picker. Selecting the model by name still works.
             */
            hidden_from_claude_code_picker: boolean;
            /**
             * Format: uuid
             * @description Stable model ID.
             */
            id: string;
            /** @description Input rate in US dollars per million tokens, as a decimal string. */
            input_price_per_million?: string | null;
            /**
             * Format: int32
             * @description Input token count above which estimates apply the long-context surcharge.
             */
            long_context_input_threshold?: number | null;
            /**
             * Format: int32
             * @description Maximum output tokens, or null when unstated.
             */
            max_output_tokens?: number | null;
            /** @description Output rate in US dollars per million tokens, as a decimal string. */
            output_price_per_million?: string | null;
            /**
             * Format: uuid
             * @description Owning provider.
             */
            provider_id: string;
            /** @description Owning provider name. */
            provider_name: string;
            /** @description How this route binds requests to provider-provisioned capacity. */
            provisioned_capacity_mode: components["schemas"]["ProvisionedCapacityMode"];
            /**
             * @description Reasoning effort tokens this model's upstream accepts, ascending by
             *     the neutral ladder rank. Null means unstated — the upstream decides;
             *     an empty array means the model takes no effort control.
             */
            supported_reasoning_efforts?: components["schemas"]["ReasoningEffort"][] | null;
            /** @description Whether the model serves the Anthropic Message Batches surface. */
            supports_anthropic_message_batches: boolean;
            /** @description Whether the model serves the Anthropic Messages surface. */
            supports_anthropic_messages: boolean;
            /** @description Whether the model serves the `OpenAI` Responses surface. */
            supports_openai_responses: boolean;
            /** @description Whether tool calls are supported. */
            supports_tools: boolean;
            /** @description Whether image input is supported. */
            supports_vision: boolean;
            /**
             * Format: int64
             * @description How many teams hold a grant to this model.
             *
             *     Zero here and in `user_grant_count` together mean nobody can route to
             *     the model: it exists in the catalog but no grant reaches it. The counts
             *     are surfaced so that state is visible on the listing instead of being
             *     discovered request by request.
             */
            team_grant_count: number;
            /** @description Ultrafast-tier one-hour cache-write rate, or null. */
            ultrafast_cache_write_1h_price_per_million?: string | null;
            /** @description Ultrafast-tier five-minute cache-write rate, or null. */
            ultrafast_cache_write_5m_price_per_million?: string | null;
            /** @description Ultrafast-tier cache-read rate, or null to use the Ultrafast input rate. */
            ultrafast_cached_input_price_per_million?: string | null;
            /**
             * @description Ultrafast-tier input rate, or null when the model has no contract rate configured.
             *
             *     Null never falls back to Fast or Standard. A served Ultrafast request stays
             *     `pending_rate` until an administrator enters this rate.
             */
            ultrafast_input_price_per_million?: string | null;
            /** @description Ultrafast-tier output rate, or null. */
            ultrafast_output_price_per_million?: string | null;
            /** @description Exact identifier sent upstream. */
            upstream_id: string;
            /**
             * Format: int64
             * @description How many people hold a direct grant to this model.
             */
            user_grant_count: number;
        };
        /**
         * @description How the gateway obtained one app's OAuth client identity (ADR 0078).
         *
         *     The three paths are not interchangeable, and which one a row took
         *     decides what an operator can do about it. A metadata-document client
         *     has nothing to rotate and no per-server registration to clean up: the
         *     client id is a URL this gateway serves, and every authorization server
         *     reads the same one. A dynamically registered client is bound to one
         *     issuer, may hold a secret that expires, and may have a management URI.
         *     A manual client is whatever a provider's console issued, and only an
         *     administrator can change it.
         * @enum {string}
         */
        OauthClientRegistration: "manual" | "dynamic_registration" | "metadata_document";
        /**
         * @description Which `OpenAI` API an OpenAI-shaped provider actually serves.
         *
         *     Distinct from [`InferenceProtocol`], which is what the client speaks to
         *     the gateway. A client always asks for Responses here; this says whether
         *     the provider on the other side answers Responses too, or whether the
         *     gateway has to translate the request into Chat Completions and the reply
         *     back. Making that a provider fact rather than a protocol variant keeps
         *     the northbound vocabulary — the thing model rows, routing, and every
         *     client-facing surface are keyed on — unchanged.
         * @enum {string}
         */
        OpenAiCompatibleApi: "responses" | "chat_completions";
        Operation: {
            body: unknown;
            method: string;
            path: string;
            result_id?: string | null;
        };
        /**
         * @description Terminal outcome for an audited operation.
         * @enum {string}
         */
        OperationOutcome: "succeeded" | "denied" | "failed";
        /**
         * @description Per-app policy for user-owned static credentials.
         * @enum {string}
         */
        PersonalCredentialPolicy: "installation_only" | "personal_preferred";
        /**
         * @description What an account can sign in with.
         *
         *     Reported because it is what decides whether `removePersonPassword` can
         *     succeed: an account holding neither would be left unable to sign in at all,
         *     and a caller can see that here rather than discovering it as a `409`.
         */
        PersonCredentials: {
            /** @description Whether a local password is set. */
            password: boolean;
            /** @description Whether a usable federated identity is linked. */
            sso: boolean;
        };
        /** @description Directory listing. */
        PersonListResponse: {
            /** @description Every directory user. */
            data: components["schemas"]["PersonView"][];
        };
        /** @description A directory user. */
        PersonView: {
            /** @description What the account can currently sign in with. */
            credentials: components["schemas"]["PersonCredentials"];
            /** @description Display name, when known. */
            display_name?: string | null;
            /** @description Email address, when known. */
            email?: string | null;
            /**
             * Format: uuid
             * @description Stable user ID.
             */
            id: string;
            /** @description Whether the account can authenticate. */
            is_active: boolean;
            /** @description Whether the account holds the administrator role. */
            is_admin: boolean;
            /**
             * @description Whether this row is a person or a service identity. Service identities
             *     appear here so membership pickers can resolve them; manage them through
             *     the service-identity operations, not the person ones.
             */
            kind: components["schemas"]["UserKind"];
            /**
             * @description Stable login handle, when one exists. Always set for service
             *     identities, which carry no email.
             */
            username?: string | null;
        };
        /**
         * @description One variable a first-party hosting preset reads, and whether this row
         *     holds it.
         *
         *     The four flags are independent facts about one variable, not a state
         *     machine, so they stay as flags on the wire.
         */
        PresetVariableView: {
            description: string;
            label: string;
            /** @description True when install generates the value instead of asking for it. */
            minted: boolean;
            name: string;
            /** @description True when the add-on does not serve without it. */
            required: boolean;
            /** @description True when the value is a credential the console must never echo. */
            secret: boolean;
            /**
             * @description True when a write has delivered this variable to the workload. Values
             *     are never readable back; only presence is.
             */
            set: boolean;
        };
        PreviewFindingView: {
            category: string;
            end: number;
            normalized: boolean;
            /** @description The redacted preview exactly as a verdict would store it. */
            preview: string;
            /**
             * @description `full_segment` for a detector kind, `bounded_context` for a configured
             *     one — the retention a real verdict would apply.
             */
            retention: string;
            rule_identity?: string | null;
            segment_kind: string;
            start: number;
        };
        /** @description A preview request: one saved instance, or an inline config to try. */
        PreviewGuardrailEngineRequest: {
            config?: Record<string, never> | null;
            /**
             * Format: uuid
             * @description Preview a saved instance exactly as admission would run it.
             */
            engine_id?: string | null;
            kind?: null | components["schemas"]["EngineKind"];
            /** @description Text to scan. Never stored. */
            sample_text: string;
            /** @description Normalized segment kind to scan as. Defaults to `message_text`. */
            segment_kind?: string | null;
            /**
             * @description Brokered surface to scan as, so a rule's surface scoping is visible in
             *     the preview rather than only in production. Defaults to
             *     `inference_request`.
             */
            surface?: string | null;
        };
        PreviewGuardrailEngineResponse: {
            data: components["schemas"]["PreviewGuardrailEngineView"];
        };
        PreviewGuardrailEngineView: {
            /**
             * @description True when the evaluation stopped at its deadline, the same signal a
             *     verdict would record as `engine_timeout`.
             */
            exhausted: boolean;
            findings: components["schemas"]["PreviewFindingView"][];
        };
        /** @description A model's rate timeline. */
        PriceHistoryResponse: {
            /** @description Every recorded interval, newest first. At most one is `scheduled`. */
            data: components["schemas"]["PriceIntervalView"][];
        };
        /** @description One interval of a model's rate timeline. */
        PriceIntervalView: {
            /** @description One-hour cache-write rate; null leaves those writes unpriced. */
            cache_write_1h_price_per_million?: string | null;
            /** @description Five-minute cache-write rate; null leaves those writes unpriced. */
            cache_write_5m_price_per_million?: string | null;
            /** @description Cache-read rate; null means cached input billed at the input rate. */
            cached_input_price_per_million?: string | null;
            /** @description Whether these are the rates in force at the time of this request. */
            effective_now: boolean;
            /** @description Fast-tier one-hour cache-write rate. */
            fast_cache_write_1h_price_per_million?: string | null;
            /** @description Fast-tier five-minute cache-write rate. */
            fast_cache_write_5m_price_per_million?: string | null;
            /** @description Fast-tier cache-read rate; null bills fast cached input at the fast input rate. */
            fast_cached_input_price_per_million?: string | null;
            /** @description Fast-tier input rate; null leaves fast-served requests awaiting a rate. */
            fast_input_price_per_million?: string | null;
            /** @description Fast-tier output rate; null leaves fast-served requests awaiting a rate. */
            fast_output_price_per_million?: string | null;
            /** @description Flex-tier one-hour cache-write rate. */
            flex_cache_write_1h_price_per_million?: string | null;
            /** @description Flex-tier five-minute cache-write rate. */
            flex_cache_write_5m_price_per_million?: string | null;
            /** @description Flex-tier cache-read rate; null bills flex cached input at the flex input rate. */
            flex_cached_input_price_per_million?: string | null;
            /** @description Flex-tier input rate; null leaves flex-served requests awaiting a rate. */
            flex_input_price_per_million?: string | null;
            /** @description Flex-tier output rate; null leaves flex-served requests awaiting a rate. */
            flex_output_price_per_million?: string | null;
            /**
             * Format: uuid
             * @description Stable interval ID. Pass it to `cancelScheduledProviderModelPrice` to
             *     withdraw an interval that has not started yet.
             */
            id: string;
            /** @description Input rate in US dollars per million tokens, as a decimal string. */
            input_price_per_million?: string | null;
            /** @description Output rate in US dollars per million tokens. */
            output_price_per_million?: string | null;
            /** @description Whether this interval has not started yet, and so can still be withdrawn. */
            scheduled: boolean;
            /** @description Ultrafast-tier one-hour cache-write rate. */
            ultrafast_cache_write_1h_price_per_million?: string | null;
            /** @description Ultrafast-tier five-minute cache-write rate. */
            ultrafast_cache_write_5m_price_per_million?: string | null;
            /** @description Ultrafast-tier cache-read rate; null uses the Ultrafast input rate. */
            ultrafast_cached_input_price_per_million?: string | null;
            /** @description Ultrafast-tier input rate; null leaves served requests awaiting a contract rate. */
            ultrafast_input_price_per_million?: string | null;
            /** @description Ultrafast-tier output rate; null leaves served requests awaiting a contract rate. */
            ultrafast_output_price_per_million?: string | null;
            /**
             * @description Inclusive RFC3339 start, or null for rates that predate versioned
             *     pricing and are therefore treated as having always applied.
             */
            valid_from?: string | null;
            /** @description Exclusive RFC3339 end, or null on the interval that runs without end. */
            valid_to?: string | null;
        };
        /**
         * @description How completely a model's rates are configured.
         *
         *     The three values partition the catalog: every model is in exactly one, so the
         *     three filtered listings concatenate back to the unfiltered one. That is what
         *     makes `unpriced` usable as "everything that cannot be billed" — whatever it
         *     omits is priced enough to bill.
         *
         *     `Partial` names a fact, not a fault. A null cache-read rate bills cached input
         *     at the input rate and null cache-write rates leave those writes unpriced, both
         *     of which are deliberate configurations for providers that do not charge for
         *     them; the filter exists to find such models, not to accuse them.
         *
         *     Carries `ToSchema` so the generated document enumerates the three legal values
         *     instead of describing them only in prose: this is what an agent reads before
         *     choosing one, and a guessed value costs it a round trip.
         * @enum {string}
         */
        PricingCompleteness: "complete" | "partial" | "unpriced";
        /**
         * @description What a pricing write did.
         * @enum {string}
         */
        PricingOutcome: "applied" | "scheduled";
        /** @description Result of a pricing write. */
        PricingUpdateView: {
            /** @description Whether the rates took effect now or were scheduled. */
            outcome: components["schemas"]["PricingOutcome"];
            /**
             * @description Whether this write also started a reconciliation pass over events
             *     already recorded for this model.
             *
             *     New rates can finalize estimates recorded before those rates existed, so
             *     an applied change reprices them immediately instead of waiting for the
             *     next periodic cycle. The pass runs after this call returns, so reported
             *     spend for this model may change shortly afterwards. A scheduled change
             *     covers a period in which no event has been recorded, so it starts none.
             */
            reconciliation_started: boolean;
        };
        /** @description Outcome of one add-on probe. */
        ProbeAddOnResponse: {
            /**
             * @description Coarse classification: `http_<status>` when a response arrived,
             *     otherwise `dns`, `connect`, `timeout`, or `other`.
             */
            detail: string;
            /** @description Whether the registered URL answered with any HTTP response at all. */
            ok: boolean;
            /** @description When the probe ran, RFC 3339. */
            probed_at: string;
        };
        /** @description Outcome of one connected-app probe. */
        ProbeConnectedAppResponse: {
            /**
             * @description Coarse classification: `http_<status>` when a response arrived,
             *     otherwise `dns`, `connect`, `tls`, `timeout`, or `other`.
             */
            detail: string;
            /** @description Whether the origin answered with any HTTP response at all. */
            ok: boolean;
            /** @description When the probe ran, RFC 3339. */
            probed_at: string;
        };
        /** @description What one provider's upstream reported. */
        ProviderDiscoveryResponse: {
            /** @description Models the upstream reported, in the order it reported them. */
            models: components["schemas"]["DiscoveredModelView"][];
            /**
             * Format: uuid
             * @description Provider the discovery ran against.
             */
            provider_id: string;
            /** @description Which upstream answered. */
            provider_kind: components["schemas"]["ModelProviderKind"];
            /** @description Administrator-facing provider name. */
            provider_name: string;
        };
        /** @description Provider listing. */
        ProviderListResponse: {
            /** @description Every configured provider. */
            data: components["schemas"]["ProviderView"][];
        };
        /**
         * @description The governed state of one provider.
         *
         *     Both policy families in one view because both are read for the same reason —
         *     establishing what this provider is currently allowed to do — and a caller
         *     that had to make two calls could act on a half-refreshed picture.
         */
        ProviderPolicyView: {
            /**
             * Format: int64
             * @description How many users currently have an active subscription attached.
             */
            active_subscription_count: number;
            /** @description Bedrock project this provider routes through, null for other kinds. */
            bedrock_mantle_project_id?: string | null;
            bedrock_retention_policy?: null | components["schemas"]["BedrockDataRetentionPolicy"];
            /**
             * Format: uuid
             * @description Provider the policies belong to.
             */
            provider_id: string;
            /**
             * @description Which upstream protocol family this provider speaks, since it decides
             *     which of these fields are meaningful.
             */
            provider_kind: string;
            /**
             * @description Whether subscription credentials may be used only by an attributed CLI
             *     session.
             */
            subscription_cli_only: boolean;
            /**
             * @description Whether a newly attached subscription starts out willing to fall back to
             *     the installation's metered credential.
             */
            subscription_fallback_default: boolean;
            /**
             * Format: int32
             * @description Remaining-capacity reserve used for account draining and shared cutover.
             */
            subscription_headroom_percent: number;
            /**
             * @description Whether users may attach their own consumer subscription to this
             *     provider.
             */
            subscription_policy: components["schemas"]["SubscriptionCredentialPolicy"];
            /** @description Whether known reset deadlines are spent before generic least-used order. */
            subscription_reset_priority_enabled: boolean;
            /**
             * @description Whether a gateway-hosted sandbox's delegated session is excluded from
             *     subscription resolution on this provider.
             */
            subscription_sandbox_excluded: boolean;
            /** @description How eligible shared capacity competes with owned subscriptions. */
            subscription_shared_routing_policy: components["schemas"]["SubscriptionSharedRoutingPolicy"];
            /** @description Whether owners may lend subscription capacity to teams. */
            subscription_sharing_enabled: boolean;
        };
        /**
         * @description Stable result of a provider reset-credit attempt.
         * @enum {string}
         */
        ProviderSubscriptionCreditResetCode: "reset" | "nothing_to_reset" | "no_credit" | "already_redeemed";
        /** @description Subscription listing. */
        ProviderSubscriptionListResponse: {
            /** @description Every binding under this provider, active and revoked, by user name. */
            data: components["schemas"]["ProviderSubscriptionView"][];
            /**
             * @description Whether an account here can consume an owner-authorized reset credit
             *     through the gateway (ADR 0051). Visibility and mutation are separate
             *     capabilities: a provider can publish usage and offer no reset.
             */
            reset_credits_supported: boolean;
            /**
             * @description Whether the gateway has a usage source for this provider kind at all
             *     (ADR 0051).
             *
             *     A provider fact, not a state: `false` means no binding here will ever
             *     carry `usage_windows`, so an empty list is the provider publishing
             *     nothing rather than an account that has served no traffic. The
             *     distinction is unrecoverable from the bindings themselves, which is why
             *     it is stated here.
             */
            usage_supported: boolean;
        };
        /** @description One team a binding is currently shared with. */
        ProviderSubscriptionShareView: {
            /**
             * Format: int64
             * @description When the owner consented, as Unix seconds.
             */
            created_at_unix_seconds: number;
            /**
             * @description Whether the team currently confers reachability. A disabled team keeps
             *     its grant and confers nothing.
             */
            team_enabled: boolean;
            /**
             * Format: uuid
             * @description Team holding the grant.
             */
            team_id: string;
            /** @description Team name. */
            team_name: string;
            /** @description Team slug, which is how the CLI names the granting team. */
            team_slug: string;
        };
        /** @description One user's subscription attached to a provider. */
        ProviderSubscriptionView: {
            /**
             * @description Which upstream account the subscription belongs to, as the provider
             *     reported it. Display only, and never a credential.
             */
            account_hint?: string | null;
            /**
             * Format: uuid
             * @description Binding ID, which `revokeProviderSubscription` takes.
             */
            binding_id: string;
            /**
             * Format: int64
             * @description Requests this binding served for someone other than its owner over the
             *     requested window (ADR 0037), which is the trailing 30 days unless
             *     `since`/`until` say otherwise. Always `0` where nothing is shared, so a
             *     non-zero value is exactly the traffic a revocation would stop.
             */
            borrowed_request_count: number;
            /**
             * @description Live cooldowns, soonest-ending first, and empty when nothing is cooling
             *     down.
             *
             *     The temporal half of `limit_state`: that field says the provider is
             *     refusing the credential, these say until when. An account whose only
             *     cooldown has passed is `cooling_down` no longer, so an empty list beside
             *     a `cooling_down` state means the next request will be attempted.
             */
            cooldowns: components["schemas"]["SubscriptionCooldownView"][];
            /**
             * @description Whether this binding falls back to the installation's metered credential
             *     when the subscription cannot serve a request.
             */
            fallback_to_metered: boolean;
            /**
             * @description The owner's own name for this account, which is also how the CLI
             *     addresses it (`--sub <owner-email>/<label>`). Empty when the owner never
             *     named it.
             */
            label: string;
            /** @description Whether the provider currently considers the credential usable. */
            limit_state: components["schemas"]["SubscriptionLimitState"];
            /**
             * Format: int64
             * @description Billed metered spend on requests this account turned away while parked,
             *     in micro-USD.
             */
            parked_fallback_cost_microusd: number;
            /**
             * Format: int64
             * @description How long this account has been continuously cooling, in seconds; null
             *     when it is not.
             *
             *     A cooldown fails silently by design: the requests it turns away fall to
             *     the installation's metered credential and succeed, so a shared account
             *     can sit offline for a day with nothing complaining. Read it with
             *     `parked_fallback_cost_microusd` — together they are the difference
             *     between a throttle working and a throttle nobody noticed.
             */
            parked_for_seconds?: number | null;
            /** @description Provider-reported plan, when the provider states one. Display only. */
            plan_hint?: string | null;
            /** @description Whether the user must reauthorize before the credential works again. */
            reauthorization_required: boolean;
            /**
             * @description Teams this binding's owner lent it to (ADR 0037). Empty unless the owner
             *     shared it; an administrator may revoke a grant but never create one.
             */
            shared_teams: components["schemas"]["ProviderSubscriptionShareView"][];
            /** @description `active` or `revoked`. */
            status: string;
            /**
             * Format: int64
             * @description When any window was last observed, as Unix seconds; null when none ever
             *     was.
             *
             *     Read it before reading the windows: a snapshot hours old describes the
             *     account as it was, and a window whose reset deadline has since passed
             *     says nothing about the quota now.
             */
            usage_snapshot_at_unix_seconds?: number | null;
            /**
             * @description The provider's own quota meters as of the last observation, empty when
             *     the provider publishes none or none has been seen.
             *
             *     Empty is ambiguous on its own — no traffic yet, or a provider that
             *     publishes nothing — which is what `usage_supported` on the response
             *     disambiguates.
             */
            usage_windows: components["schemas"]["SubscriptionUsageWindowView"][];
            /** @description That user's email, when the directory has one. */
            user_email?: string | null;
            /**
             * Format: uuid
             * @description User who attached the subscription.
             */
            user_id: string;
            /** @description That user's display label. */
            user_name: string;
        };
        /** @description A provider in the model catalog. */
        ProviderView: {
            /** @description How the installation authenticates to this provider. */
            auth_method: string;
            /** @description Whether models under this provider may serve traffic. */
            enabled: boolean;
            /**
             * Format: uuid
             * @description Stable provider ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Number of catalog models owned by this provider.
             */
            model_count: number;
            /** @description Administrator-facing name. */
            name: string;
            /**
             * @description Which `OpenAI` API this provider is served southbound: `responses` or
             *     `chat_completions`, the latter translated by the gateway. Null for every
             *     kind that has no such choice, which is every kind but
             *     `openai_compatible`. Change it with `setModelProviderOpenAiApi` (ADR
             *     0031).
             * @example chat_completions
             */
            openai_compatible_api?: string | null;
            /** @description Which upstream protocol family this provider speaks. */
            provider_kind: string;
        };
        /** @description Request to create an add-on's own GitHub App forge. */
        ProvisionAddOnForgeRequest: {
            /** @description Name of the connected app; the add-on's display name when omitted. */
            name?: string | null;
        };
        /** @description The forge an add-on now holds through its service identity. */
        ProvisionedAddOnForgeResponse: {
            /**
             * Format: uuid
             * @description The new `git_forge` connected app.
             */
            app_id: string;
            /**
             * Format: uuid
             * @description The add-on's MCP endpoint the forge was attached to, when the registry
             *     names one that exists.
             */
            endpoint_id?: string | null;
            /**
             * Format: uuid
             * @description The service identity that holds it directly.
             */
            identity_id: string;
            /** @description Its name. */
            name: string;
        };
        /**
         * @description Proof retained on an inference event classified as provisioned capacity.
         * @enum {string}
         */
        ProvisionedCapacityEvidence: "provider_response" | "route_resource" | "dedicated_request";
        /**
         * @description How one model route binds requests to provider-provisioned capacity.
         * @enum {string}
         */
        ProvisionedCapacityMode: "none" | "provider_observed" | "route_resource" | "dedicated_only" | "metered_only";
        ProvisionRuntimeRequest: {
            apply?: boolean;
            /**
             * Format: uuid
             * @description Existing single-app clients may keep sending this field.
             */
            github_app_id?: string | null;
            /** @description Explicit execution connections. Supply this or `github_app_id`, never both. */
            github_app_ids?: string[] | null;
            /** @description Optional verified digest for automation. The console resolves the hosted release. */
            image?: string | null;
        };
        ProvisionRuntimeResponse: {
            applied: boolean;
            image: string;
            operations: components["schemas"]["Operation"][];
            runtime: components["schemas"]["RuntimeView"];
        };
        /** @description Publish a signed snapshot for a site. */
        PublishConnectorSnapshotRequest: {
            /** @description Sessions the connector must close. */
            kills?: string[];
            /** @description Logical mappings onto ceiling targets. Absence of a previous mapping is revocation. */
            mappings: Record<string, never>[];
        };
        /** @description Newly signed snapshot. */
        PublishedConnectorSnapshot: {
            /**
             * Format: int64
             * @description Monotonic sequence.
             */
            seq: number;
            /**
             * Format: uuid
             * @description Site.
             */
            site_id: string;
            /** @description Gateway-signed envelope the connector verifies against its ceiling. */
            snapshot: Record<string, never>;
        };
        /** @description Request to publish a shared app to one of the author's teams. */
        PublishSharedAppRequest: {
            /**
             * Format: uuid
             * @description Team whose members the app becomes reachable by.
             */
            team_id: string;
        };
        RateLimitListResponse: {
            data: components["schemas"]["RateLimitView"][];
        };
        RateLimitResponse: {
            data: components["schemas"]["RateLimitView"];
        };
        /** @enum {string} */
        RateLimitScopeType: "installation" | "user" | "team" | "gateway_model" | "connected_app";
        RateLimitTemplateListResponse: {
            data: components["schemas"]["RateLimitTemplateView"][];
        };
        /** @description One suggested policy inside a template. */
        RateLimitTemplatePolicyView: {
            dimension: string;
            /** Format: int64 */
            limit_value: number;
            /** @description Why this bound, stated so an administrator can adjust it. */
            rationale: string;
            scope_type: string;
        };
        /** @description A curated starting point an administrator applies explicitly. */
        RateLimitTemplateView: {
            description: string;
            /** @description Stable template identifier. */
            id: string;
            name: string;
            policies: components["schemas"]["RateLimitTemplatePolicyView"][];
        };
        RateLimitView: {
            created_at: string;
            dimension: string;
            enabled: boolean;
            /**
             * @description When this policy was last recorded exhausted, if it still is. A
             *     per-minute window clears the marker on its next successful admission,
             *     so a value here means the window was exhausted and has admitted nothing
             *     since; a concurrency policy's marker re-arms rather than clearing, so it
             *     reads as the most recent denial and `in_flight` is the live gauge.
             */
            exceeded_at?: string | null;
            /** Format: uuid */
            id: string;
            /**
             * Format: int64
             * @description Requests currently holding a slot on this node. Present only for
             *     concurrency policies; multi-node deployments may drift by up to one
             *     node's allowance (ADR 0054).
             */
            in_flight?: number | null;
            /** Format: int64 */
            limit_value: number;
            /** Format: uuid */
            scope_id?: string | null;
            scope_label: string;
            scope_type: string;
            source: string;
            updated_at: string;
            /**
             * Format: double
             * @description Fraction of the per-minute window currently consumed, `0..=1`. Absent
             *     for concurrency policies, whose live gauge is `in_flight`.
             */
            utilization?: number | null;
        };
        /**
         * @description Provider-neutral reasoning effort requested for a sandbox harness.
         *
         *     A first-party harness clamps this request to the nearest supported
         *     level without exceeding it. A request below its ladder applies no
         *     explicit effort control. A BYO custom harness receives the exact token
         *     through its environment because the gateway cannot know its ladder.
         * @enum {string}
         */
        ReasoningEffort: "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra";
        /** @description Request to replace one gateway model's waterfall. */
        ReplaceWaterfallRequest: {
            /**
             * @description Whether the fallbacks are consulted. Omitted leaves the stored value
             *     unchanged; a model that has never been configured defaults to true.
             */
            enabled?: boolean | null;
            policy?: null | components["schemas"]["FailoverPolicyView"];
            /**
             * @description Fallback tiers in priority order. Array order *is* the priority, and the
             *     list replaces whatever was configured: an empty list clears the
             *     waterfall.
             */
            routes: components["schemas"]["WaterfallRouteInput"][];
            /**
             * @description Whether upstream throttling triggers failover. Omitted leaves the stored
             *     value unchanged; the default is false, meaning outages only.
             */
            trigger_on_rate_limit?: boolean | null;
        };
        /** @description Owner-authorized reset-credit consumption request. */
        ResetProviderSubscriptionCreditsRequest: {
            /** @description Optional provider credit ID to consume; must contain 1 to 256 bytes after trimming. */
            credit_id?: string | null;
            /**
             * @description Caller-generated idempotency key, forwarded as the provider redemption request ID.
             *     Must contain 1 to 128 bytes after trimming.
             */
            redeem_request_id: string;
        };
        /** @description Result returned after consuming a provider reset credit. */
        ResetProviderSubscriptionCreditsResponse: {
            /** @description Provider-normalized outcome. */
            code: components["schemas"]["ProviderSubscriptionCreditResetCode"];
            /**
             * Format: int64
             * @description Number of quota windows reset by the provider.
             */
            windows_reset: number;
        };
        /** @description The governed REST projection an app's operations are exposed through. */
        RestApiConnectionInput: {
            agent_access_mode?: null | components["schemas"]["ConnectedAppAccessMode"];
            /** @description Whether the app may be called at a private-network address. */
            allow_private_networks?: boolean;
            /** @description Header the credential is injected into, for `header` placement. */
            credential_header_name?: string | null;
            credential_placement?: null | components["schemas"]["CredentialPlacement"];
            /**
             * Format: int32
             * @description Largest request body in bytes, 1024 to 10485760. Defaults to 1048576.
             */
            max_request_bytes?: number | null;
            /**
             * Format: int32
             * @description Largest response body in bytes, 1024 to 10485760. Defaults to 1048576.
             */
            max_response_bytes?: number | null;
            /**
             * @description The `OpenAPI` document defining the callable operations.
             *
             *     Required for `rest_api`; ignored for `datadog` and `sentry`, whose
             *     documents ship with the gateway.
             */
            openapi_document?: string | null;
            /**
             * Format: int32
             * @description Per-request timeout in seconds, 1 to 120. Defaults to 30.
             */
            timeout_seconds?: number | null;
            /** @description MCP tool slug the projection is addressed by. Unique across apps. */
            tool_slug?: string | null;
        };
        ReviewGuardrailFindingRequest: {
            note?: string | null;
            reason?: null | components["schemas"]["FindingReviewReason"];
            status: components["schemas"]["FindingReviewStatus"];
        };
        /**
         * @description The rotation result. The plaintext appears exactly once, and only for
         *     external rows: a managed row's secret is written straight into the
         *     workload's Kubernetes Secret (ADR 0095 decision 5), so no response body —
         *     and no administrator — ever carries it.
         */
        RotatedSecretResponse: {
            client_secret?: string | null;
        };
        /** @description Live route-affinity pins, most recently served first. */
        RouteAffinityListResponse: {
            data: components["schemas"]["RouteAffinityPinView"][];
        };
        /** @description One live route-affinity pin. */
        RouteAffinityPinView: {
            /**
             * Format: int32
             * @description The provider prompt-cache lifetime the pinned route's own usage
             *     reporting revealed, in seconds. Null when no response ever named one.
             */
            cache_ttl_seconds?: number | null;
            /**
             * Format: uuid
             * @description The asserted conversation, present only for `conversation` pins.
             */
            conversation_id?: string | null;
            /**
             * @description Whether the pin's cache-warmth evidence is scoped to a recorded
             *     serving credential. The credential itself is never exposed.
             */
            credential_scoped: boolean;
            /**
             * @description A short hex prefix of the derived key's digest, present only for
             *     `derived` pins. Purely a display identifier: the digest is a keyed
             *     HMAC over the request's stable head and never leaves storage whole.
             */
            derived_key_prefix?: string | null;
            /** @description When the pin lapses back to ordinary waterfall order, RFC3339. */
            expires_at: string;
            /**
             * @description Which key kind holds the pin: `conversation` for identity asserted in
             *     attribution headers, `derived` for the ephemeral digest of a
             *     headerless request's stable head.
             * @example conversation
             */
            key_kind: string;
            /** @description Administrator-facing name of the gateway model. */
            primary_display_name: string;
            /**
             * Format: uuid
             * @description Gateway model whose waterfall the pin is scoped to.
             */
            primary_model_id: string;
            /** @description When the pinned route last served, RFC3339. */
            served_at: string;
            /** @description Administrator-facing name of the serving route. */
            serving_display_name: string;
            /**
             * Format: uuid
             * @description Route that last served the key, preferred while the pin holds.
             */
            serving_model_id: string;
        };
        /**
         * @description Whether a configured tier can be selected right now, and what holds it out.
         *
         *     The route table's own `enabled` bit is not the whole answer: selection reads
         *     the catalog row and its provider too, so a tier switched on in the waterfall
         *     whose provider was later disabled is configuration that looks live and is
         *     not. Reporting the two together is what lets an administrator see, before an
         *     outage, that a model has no fallback left.
         * @enum {string}
         */
        RouteAvailabilityView: "available" | "route_disabled" | "model_disabled" | "provider_disabled";
        /** @description One route's observed health. */
        RouteHealthView: {
            /**
             * @description Badge vocabulary shared with the catalog's effective state: `ready`,
             *     `warning`, or `danger`.
             */
            class: string;
            /**
             * @description Stable slug: `healthy`, `degraded`, or `circuit_open`.
             * @example circuit_open
             */
            code: string;
            /** @description What the state means for traffic right now. */
            detail: string;
            /** @description Short badge text. */
            label: string;
        };
        /**
         * @description Where a route sits relative to the model's publisher.
         * @enum {string}
         */
        RouteSourceClassView: "direct" | "hyperscaler" | "aggregator";
        /** @description Runner-class listing. */
        RunnerClassListResponse: {
            /** @description Classes smallest rank first. */
            data: components["schemas"]["RunnerClassView"][];
        };
        /** @description One operator-owned runner class (ADR 0109): a size and the pool that has it. */
        RunnerClassView: {
            /**
             * Format: int32
             * @description CPU ceiling in millicores.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: uuid
             * @description Class ID.
             */
            id: string;
            /**
             * Format: int32
             * @description Memory ceiling in MiB.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /** @description Slug-shaped name profiles and spawns select by. */
            name: string;
            /** @description Node selector naming the pool that has machines of this size. */
            node_selector: unknown;
            /** @description Live profiles currently permitting this class, by name. */
            profile_names: string[];
            /**
             * Format: int32
             * @description Smallest-to-largest order; unique across the catalogue.
             */
            rank: number;
            /**
             * Format: int32
             * @description Optional wall-clock default the class suggests.
             */
            wall_clock_timeout_seconds?: number | null;
        };
        RuntimeApp: {
            /** Format: uuid */
            id: string;
            name: string;
        };
        RuntimeView: {
            /** Format: int32 */
            applied_manifest_version?: number | null;
            /** Format: uuid */
            github_app_id?: string | null;
            github_app_ids: string[];
            github_apps: components["schemas"]["RuntimeApp"][];
            image?: string | null;
            /** Format: int32 */
            manifest_version: number;
            /** @description `machine`, `converging`, `sandbox`, or `unavailable`, from delivered settings and live resources. */
            placement: string;
            ready: boolean;
            reason?: string | null;
            release_tag?: string | null;
        };
        /** @description Sibling app-HTTP list for one sandbox. */
        SandboxAppHttpListView: {
            /** @description Newest first. At most 500. */
            data: components["schemas"]["SandboxAppHttpView"][];
        };
        /** @description One sandbox-attributed governed app HTTP request (ADR 0044 validation #4). */
        SandboxAppHttpView: {
            /** @description Connected-app name. */
            app_name: string;
            /** @description Elided shape of a path no approved operation matched, on a refusal. */
            attempted_path_shape?: string | null;
            /** @description Resolved HTTP method. */
            http_method: string;
            /**
             * Format: uuid
             * @description Usage event ID.
             */
            id: string;
            /** @description UTC event timestamp, RFC3339. */
            occurred_at: string;
            /**
             * @description Audit detail for body-decided operations (ADR 0067): for
             *     `graphqlRequest`, the top-level `type:field` names.
             */
            operation_detail?: unknown;
            /** @description Canonical operation ID, when one was recorded. */
            operation_id?: string | null;
            /** @description Approved path template without concrete values. */
            operation_path_template?: string | null;
            /** @description Succeeded, denied, or failed. */
            outcome: string;
            /**
             * Format: int32
             * @description Upstream HTTP status when applicable.
             */
            upstream_status?: number | null;
        };
        /**
         * @description Why the cluster cannot place a sandbox pod.
         *
         *     Closed so a caller can branch on the class without reading node names
         *     out of the scheduler's `FitError`. The three values are the classes in
         *     the #1611 incident; anything else is reported as no class rather than
         *     as an open string.
         * @enum {string}
         */
        SandboxCapacityClass: "ephemeral_storage" | "affinity" | "taints";
        SandboxConcurrencyResponse: {
            data: components["schemas"]["SandboxConcurrencyView"];
        };
        /**
         * @description One installation's live-sandbox allowance and what is currently using it.
         *
         *     Both halves in one view because a caller at a cap has to know which one bit
         *     before it knows what to do: a personal ceiling clears when you cancel your
         *     own run, and the installation's clears when somebody else's finishes.
         *
         *     The occupancy is split as well as totalled. A sandbox the cluster has not
         *     placed holds its slot exactly as a running one does, so it counts in
         *     `user_running` — but it is not work under way, and #1611 is the case where
         *     eight "running" sandboxes were four runs and four pods no node would accept.
         *     `user_awaiting_scheduling` is the subset that says so, and
         *     `scheduling_pressure` names the class the scheduler gave.
         */
        SandboxConcurrencyView: {
            /**
             * Format: int64
             * @description Of [`Self::installation_running`], how many are waiting for a node.
             */
            installation_awaiting_scheduling: number;
            /** @description Whether the installation ceiling is full for everyone. */
            installation_exhausted: boolean;
            /**
             * Format: int32
             * @description Most live sandboxes this installation runs, across all users.
             */
            installation_limit: number;
            /**
             * Format: int64
             * @description Live sandboxes across the installation that a node has accepted.
             */
            installation_placed: number;
            /**
             * Format: int64
             * @description Live sandboxes across the installation now, placed or not.
             */
            installation_running: number;
            schedulable?: null | components["schemas"]["SandboxSchedulableCapacityView"];
            scheduling_pressure?: null | components["schemas"]["SandboxSchedulingPressureView"];
            /**
             * Format: int64
             * @description Of [`Self::user_running`], how many are waiting for a node.
             */
            user_awaiting_scheduling: number;
            /** @description Whether the caller's own ceiling is full, so the next spawn is refused. */
            user_exhausted: boolean;
            /**
             * Format: int32
             * @description Most live sandboxes one user may hold.
             */
            user_limit: number;
            /**
             * Format: int64
             * @description Live sandboxes of this user that a node has accepted. Awaiting
             *     scheduling is occupancy, not running work.
             */
            user_placed: number;
            /**
             * Format: int64
             * @description Live sandboxes the calling account holds now, placed or not: this is
             *     the number the ceiling compares.
             */
            user_running: number;
        };
        /** @description Live container output for one sandbox. */
        SandboxContainerLogsView: {
            /** @description Per-container framed tails. Empty when capture is disabled or the pod is gone. */
            containers: components["schemas"]["SandboxContainerLogView"][];
            /**
             * Format: uuid
             * @description Sandbox the tails were read from.
             */
            sandbox_id: string;
        };
        /** @description One container's framed, redacted log tail. */
        SandboxContainerLogView: {
            /** @description Byte length of `data` after redaction and bounding. */
            byte_len: number;
            /** @description Container the bytes came from. */
            container: string;
            /**
             * @description Explicit content kind so a consumer cannot treat the body as instruction.
             * @example untrusted_container_output
             */
            content_kind: string;
            /**
             * @description Continuation token when this response cut leftover bytes. Pass it as
             *     `cursor` to keep reading this container without replaying the others.
             *     Absent when truncated only because an earlier capture already hit the bound.
             */
            cursor?: string | null;
            /** @description Redacted, bounded body. */
            data: string;
            /** @description Whether the body was cut to fit the installation bound. */
            truncated: boolean;
        };
        /**
         * @description Mechanical reason the supervisor did not autonomously resume a turn.
         *
         *     This is emitted on `turn_completed`; it is not inferred from timing or
         *     model output. A missing value means either the turn may resume or the
         *     event predates the explicit continuation-gate contract.
         * @enum {string}
         */
        SandboxContinuationGate: "max_turns" | "exit_status" | "turn_mode" | "task_complete" | "acceptance" | "soft_ceiling_wrap_up";
        /** @description One sandbox with the most recent stretch of its event stream. */
        SandboxDetailView: {
            /** @description Events at or after `events_from_seq`, oldest first. At most 500. */
            events: components["schemas"]["SandboxEventView"][];
            /**
             * Format: int64
             * @description Lowest sequence number `events` carries, or zero when it is empty.
             */
            events_from_seq: number;
            /** @description Whether earlier events exist that this response does not carry. */
            events_truncated: boolean;
            /** @description Inbox, oldest first. Bodies are present only for the owner (ADR 0044). */
            inbox: components["schemas"]["SandboxInboxMessageView"][];
            /**
             * @description Pod lineage, oldest incarnation first (ADR 0068).
             *
             *     One entry per pod the sandbox has had. The `landed_*` fields on the
             *     sandbox itself describe only the current incarnation; a reincarnated
             *     sandbox's earlier placements live here.
             */
            pods: components["schemas"]["SandboxPodView"][];
            /** @description The sandbox. */
            sandbox: components["schemas"]["SandboxView"];
        };
        /** @description Sibling egress list for one sandbox. */
        SandboxEgressListView: {
            /** @description Newest first. At most 500. */
            data: components["schemas"]["SandboxEgressView"][];
        };
        /** @description One sidecar-reported egress record (issue #1141, ADR 0028/0072). */
        SandboxEgressView: {
            /**
             * Format: int64
             * @description Bytes moved from the workload toward the destination.
             */
            bytes_to_destination?: number | null;
            /**
             * Format: int64
             * @description Bytes moved from the destination back to the workload.
             */
            bytes_to_workload?: number | null;
            /** @description Exact destination authority. */
            destination: string;
            /**
             * Format: int64
             * @description Connection or request lifetime in milliseconds.
             */
            duration_ms?: number | null;
            /**
             * Format: int64
             * @description Insertion-ordered record ID.
             */
            id: number;
            /**
             * @description Record class: `tunnel_closed`, `tunnel_failed`, `intercepted_request`,
             *     `intercepted_request_failed`, or `destination_refused`.
             */
            kind: string;
            /** @description HTTP method, on intercepted-request records. */
            method?: string | null;
            /** @description UTC receipt timestamp, RFC3339. */
            occurred_at: string;
            /** @description Request path without its query string, on intercepted-request records. */
            path?: string | null;
            /** @description Stable failure or refusal reason, on the failure classes. */
            reason?: string | null;
            /**
             * Format: int32
             * @description Upstream status relayed to the workload.
             */
            status?: number | null;
        };
        /** @description One durable sandbox progress event. */
        SandboxEventView: {
            /** @description UTC event timestamp, RFC3339. */
            created_at: string;
            /**
             * @description Event kind, lowercase and underscore-separated.
             * @example harness_started
             */
            kind: string;
            /** @description Event payload object. */
            payload: unknown;
            /**
             * Format: int64
             * @description Per-sandbox monotonic, gap-free sequence number.
             */
            seq: number;
        };
        /** @description One steering message on a sandbox's inbox. */
        SandboxInboxMessageView: {
            /** @description Message body. Absent when the caller is not the owner. */
            body?: string | null;
            /** @description UTC timestamp the message was appended. */
            created_at: string;
            /** @description Whether the supervisor has acknowledged delivery. */
            delivered: boolean;
            /** @description Whether this message preempts the turn in flight. */
            interrupt: boolean;
            /**
             * Format: uuid
             * @description User who sent it.
             */
            sender_user_id: string;
            /**
             * Format: int64
             * @description Per-sandbox monotonic sequence number.
             */
            seq: number;
        };
        /** @description Sibling inference list for one sandbox. */
        SandboxInferenceListView: {
            /** @description Newest first. At most 500. */
            data: components["schemas"]["SandboxInferenceView"][];
        };
        /** @description One sandbox-attributed inference request (ADR 0044 validation #4). */
        SandboxInferenceView: {
            /**
             * Format: uuid
             * @description Conversation attributed to this request, when the harness supplied one.
             */
            conversation_id?: string | null;
            /**
             * Format: int64
             * @description Estimated request cost in micro-US-dollars.
             */
            estimated_cost_microusd?: number | null;
            /** @description Client-visible model ID. */
            gateway_model_id: string;
            /**
             * Format: uuid
             * @description Usage event ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens?: number | null;
            /** @description UTC event timestamp, RFC3339. */
            occurred_at: string;
            /** @description Succeeded, denied, or failed. */
            outcome: string;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens?: number | null;
            /** @description Provider display snapshot. */
            provider_name: string;
        };
        /** @description Sandbox listing. */
        SandboxListResponse: {
            /** @description Matching sandboxes, newest first. */
            data: components["schemas"]["SandboxView"][];
        };
        /** @description The receipt one steering message produced. */
        SandboxMessageReceiptView: {
            /** @description Whether it will preempt the turn in flight. */
            interrupt: boolean;
            /**
             * Format: int64
             * @description Messages still ahead of the agent, including this one.
             */
            pending_messages: number;
            /**
             * Format: uuid
             * @description Sandbox the message was appended to.
             */
            sandbox_id: string;
            /**
             * Format: int64
             * @description Per-sandbox monotonic sequence number this message was recorded at.
             */
            seq: number;
        };
        /**
         * @description Derived watch-and-steer phase of one sandbox (ADR 0044).
         *
         *     Never persisted. Computed from the row, the event stream, and
         *     `pending_messages`. The last eight variants are the lifecycle state
         *     itself — the total catch-all, not a fifth terminal case.
         * @enum {string}
         */
        SandboxPhase: "awaiting_scheduling" | "initializing" | "bootstrapping" | "preparing_workspace" | "working" | "delivering" | "idle" | "pending" | "provisioning" | "running" | "completing" | "completed" | "failed" | "cancelled" | "expired" | "ceiling_exceeded";
        /** @description One pod incarnation of a sandbox (ADR 0068). */
        SandboxPodView: {
            /** @description Namespace-qualified pod identity, once the backend accepted it. */
            backend_reference?: string | null;
            /** @description Closed-word reason the pod ended. Absent for the live pod. */
            end_reason?: string | null;
            /** @description UTC timestamp the pod ended, RFC3339. Absent for the live pod. */
            ended_at?: string | null;
            /**
             * Format: int32
             * @description Monotone incarnation number, starting at 1.
             */
            incarnation: number;
            /** @description Capacity type of that node: `spot`, `on-demand`, or `unknown`. */
            landed_capacity_type?: string | null;
            /** @description Instance type of that node. */
            landed_instance_type?: string | null;
            /** @description Node the pod landed on, when the landing was snapshotted in time. */
            landed_node_name?: string | null;
            /** @description Availability zone of that node. */
            landed_zone?: string | null;
            /** @description UTC timestamp the pod was accepted, RFC3339. */
            started_at?: string | null;
            /** @description The substrate's `pod.status.reason` at the end, when it offered one. */
            terminal_pod_reason?: string | null;
        };
        /** @description Sandbox profile listing. */
        SandboxProfileListResponse: {
            /** @description Matching profiles, in name order. */
            data: components["schemas"]["SandboxProfileView"][];
        };
        /**
         * @description How a profile's workload is started (ADR 0048).
         *
         *     Not a Kubernetes `RuntimeClass` — those stay the isolation axis.
         * @enum {string}
         */
        SandboxProfileRuntime: "first_party" | "byo";
        /** @description One administrator-authored sandbox profile. */
        SandboxProfileView: {
            /** @description Execution substrate. Only `kubernetes` ships today. */
            backend: string;
            /** @description Named image-contract violations when conformance failed. */
            conformance_failures: string[];
            /** @description ADR 0073 toolchain-image state: pending, passed, failed, or `not_required`. */
            conformance_status: string;
            /** @description Connector-terminated connections this profile may use. */
            connector_connection_ids: string[];
            /**
             * Format: int32
             * @description CPU ceiling in millicores.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * @description The class a spawn gets when it names none; `null` exactly when
             *     `runner_classes` is empty.
             */
            default_runner_class?: string | null;
            /** @description Whether a sandbox may be spawned from this profile. */
            enabled: boolean;
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: int32
             * @description Seconds of inactivity while the client remains in the foreground (ADR 0111
             *     decision 6). `None` keeps the ordinary idle timer.
             */
            foreground_idle_timeout_seconds?: number | null;
            /**
             * Format: uuid
             * @description Stable profile ID.
             */
            id: string;
            /**
             * Format: int32
             * @description Seconds of inactivity before the sandbox is stopped.
             */
            idle_timeout_seconds: number;
            /** @description Digest-pinned workload image. */
            image: string;
            /**
             * Format: int32
             * @description Pod incarnations one sandbox may consume (ADR 0068 part 3).
             *
             *     A circuit breaker on reincarnation after infrastructure loss;
             *     1 disables reincarnation for the profile.
             */
            max_pod_incarnations: number;
            /**
             * Format: int32
             * @description Memory ceiling in MiB.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /** @description Slug-shaped name clients select the profile by. */
            name: string;
            /** @description Node placement selector; an empty object constrains nothing. */
            node_selector: unknown;
            /** @description Egress bundles a spawn may add by naming them, and only these. */
            optional_egress_bundles: string[];
            /** @description Whether a spawn may declare a pinned repository (and therefore GitHub). */
            permits_repositories: boolean;
            /** @description Harnesses this profile permits, in stable order. */
            permitted_harnesses: components["schemas"]["ConversationHarness"][];
            /**
             * @description Gateway model IDs this profile permits, in stable order. Empty declares
             *     no model restriction.
             */
            permitted_model_routes: string[];
            /** @description Annotations rendered onto this profile's pods; an empty object adds none. */
            pod_annotations: unknown;
            /** @description How the workload is started (ADR 0048). Not a Kubernetes `RuntimeClass`. */
            profile_runtime: components["schemas"]["SandboxProfileRuntime"];
            /** @description Egress bundles always in this profile's effective tunnel set, by name. */
            required_egress_bundles: string[];
            /**
             * @description Runner classes this profile permits, smallest rank first (ADR 0109).
             *     Empty means the profile sizes every spawn from its own inline columns.
             */
            runner_classes: string[];
            /** @description Hardened runtime class — gVisor or equivalent. */
            runtime_class: string;
            /**
             * Format: int64
             * @description Per-sandbox ceiling in micro-US-dollars of shadow model cost (all billing classes).
             */
            spend_ceiling_microusd: number;
            /** @description Taints this profile's pods tolerate; an empty array tolerates none. */
            tolerations: unknown;
            /** @description Exact-match tunnel domains, each with its egress mode. */
            tunnel_domains: components["schemas"]["TunnelDomainView"][];
            /**
             * Format: int32
             * @description Total seconds a sandbox may live.
             */
            wall_clock_timeout_seconds: number;
            /**
             * @description Repository reference whose warm layer the builder refreshes on the paired
             *     schedule (ADR 0111 decision 8). `None` means cold starts.
             */
            warm_image_reference?: string | null;
            /** @description Schedule for refreshing [`Self::warm_image_reference`], accepted only beside it. */
            warm_image_schedule?: string | null;
        };
        /** @description One git repository declared for a sandbox. */
        SandboxRepositoryView: {
            /** @description Branch or commit started from. */
            repository_ref?: string | null;
            /** @description HTTPS URL as declared at spawn. */
            url: string;
        };
        /**
         * @description How a sandbox continues after a successful headless turn (ADR 0047).
         *
         *     `task` remains the only prompt. This is a continuation policy, not a
         *     second goal string, and it is refused for any harness without a
         *     verified resume.
         * @enum {string}
         */
        SandboxRunMode: "turn" | "goal";
        /** @description Cluster capacity inferred from a scheduler `FitError`. */
        SandboxSchedulableCapacityView: {
            /** @description Whether a scaler named itself in the `FitError`. Absent when it did not. */
            autoscaling_active?: boolean | null;
            /**
             * Format: int64
             * @description How many more sandbox pods the observation says can be placed.
             */
            available: number;
            limiting_class?: null | components["schemas"]["SandboxCapacityClass"];
            /**
             * Format: int32
             * @description Seconds to wait before retrying a spawn.
             */
            retry_after_seconds: number;
        };
        /**
         * @description Why the cluster has not placed the sandboxes that are waiting.
         *
         *     The scheduler's own vocabulary, not one this gateway invents: the reason is
         *     the `PodScheduled` condition (`Unschedulable`, `SchedulingGated`,
         *     `SchedulerError`) and the message is the detail it gave. Reported
         *     installation-wide, because a cluster with nowhere to put a pod is not a
         *     per-user fact.
         */
        SandboxSchedulingPressureView: {
            /** @description The detail it gave alongside, when it gave one. */
            message?: string | null;
            /**
             * @description The scheduler's condition reason on the most recently updated waiting
             *     sandbox.
             */
            reason: string;
        };
        /**
         * @description What per-sandbox `spend_microusd` and the spend ceiling measure.
         *
         *     Distinct from [`crate::BillingClass`], which classifies one ledger row's
         *     payer. This names the sum the ceiling compares against. Organizational
         *     cost limits count `billed` rows only and deny the next request; the
         *     sandbox ceiling sums every class at notional model price and terminates
         *     the run. That is a resource bound, not an invoice bound (#977).
         * @enum {string}
         */
        SandboxSpendBasis: "model_cost";
        /**
         * @description Desired and observed lifecycle state of one gateway-hosted sandbox.
         *
         *     Terminal states are exactly those for which [`SandboxState::is_terminal`]
         *     holds. There is no snapshot, resume, or migration: a sandbox that dies
         *     transitions to [`SandboxState::Failed`] with its event stream intact up
         *     to the last persisted sequence number, and the user respawns.
         *
         *     The terminal states partition *outcomes*, not causes, and the partition
         *     is the point (#976): a bound the installation configured is not a
         *     malfunction, so reaching one never reads as [`SandboxState::Failed`].
         *     Five terminal words — the work finished, the operator stopped it, the
         *     wall clock elapsed, a spend bound was reached, or something broke — with
         *     `termination_reason` and `failure_reason` carrying which bound and which
         *     breakage.
         * @enum {string}
         */
        SandboxState: "pending" | "provisioning" | "running" | "completing" | "completed" | "failed" | "cancelled" | "expired" | "ceiling_exceeded";
        /**
         * @description Why a sandbox was marked for termination before it finished on its own.
         *
         *     Termination is *requested* on the row and consumed by the reconciler,
         *     which is what makes "the workload stops, not merely its network" true
         *     for a revocation that arrives while the client is offline. The reason is
         *     recorded because the triggers are not interchangeable to an auditor:
         *     a user cancelling their own run and an administrator revoking a grant
         *     underneath it are different stories about the same terminal state.
         *
         *     [`SandboxTerminationReason::SchedulingTimeout`] is the one value the
         *     sweep discovers rather than a caller requesting, and it is a member here
         *     rather than a bare sweep string because it settles a sandbox that never
         *     ran: the state it produces and the words it produces are decided by the
         *     same table every requested termination goes through, so the reconciler
         *     and the sidecar's heartbeat cannot disagree about it either.
         * @enum {string}
         */
        SandboxTerminationReason: "cancelled" | "idle_ceiling" | "grant_revoked" | "user_disabled" | "spend_ceiling_exceeded" | "user_daily_budget_exceeded" | "installation_daily_budget_exceeded" | "scheduling_timeout" | "inference_unavailable";
        /** @description Inference spend attributed to one sandbox. */
        SandboxUsageView: {
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Reserved CPU-millicore-seconds. Resource-seconds, not dollars.
             */
            cpu_request_milli_seconds: number;
            /** Format: int64 */
            credits_cost_microusd: number;
            /** Format: int64 */
            estimated_cost_microusd: number;
            /** Format: int64 */
            inference_requests: number;
            /** Format: int64 */
            input_tokens: number;
            last_activity_at?: string | null;
            /**
             * Format: int64
             * @description Reserved memory-MiB-seconds. Resource-seconds, not dollars.
             */
            memory_mib_seconds: number;
            /** Format: int64 */
            output_tokens: number;
            /** Format: int64 */
            pending_rate_requests: number;
            /** Format: int64 */
            priced_requests: number;
            /** Format: int64 */
            provisional_requests: number;
            /** Format: int64 */
            provisioned_cost_microusd: number;
            /** Format: uuid */
            sandbox_id: string;
            /** Format: int64 */
            subscription_cost_microusd: number;
            /** Format: int64 */
            unpriceable_requests: number;
            /** Format: uuid */
            user_id: string;
        };
        /** @description One gateway-hosted sandbox. */
        SandboxView: {
            /** @description Namespace-qualified backend object identity, once provisioned. */
            backend_reference?: string | null;
            /** @description UTC completion timestamp, present exactly for terminal states. */
            completed_at?: string | null;
            continuation_gate?: null | components["schemas"]["SandboxContinuationGate"];
            /**
             * Format: int32
             * @description CPU limit in millicores, snapshotted at spawn.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int64
             * @description Reserved CPU-millicore-seconds (wall duration × profile request).
             */
            cpu_request_milli_seconds: number;
            /**
             * Format: int32
             * @description CPU request in millicores, snapshotted at spawn from the class or the
             *     profile's inline size (ADR 0109).
             */
            cpu_request_millicores: number;
            /** @description UTC creation timestamp, RFC3339. */
            created_at: string;
            effective_reasoning_effort?: null | components["schemas"]["ReasoningEffort"];
            /** @description Optional egress bundles the spawn named, in declared order. */
            egress_bundles: string[];
            /**
             * Format: uuid
             * @description Endpoint the spawn was authorized through.
             */
            endpoint_id: string;
            /**
             * Format: int32
             * @description Ephemeral storage limit in MiB, snapshotted at spawn; the bound the
             *     `ephemeral_storage` events in the stream measure against.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: uuid
             * @description The sandbox's own long-lived sandbox-kind runtime execution.
             */
            execution_id: string;
            /**
             * @description UTC wall-clock deadline, RFC3339.
             *
             *     Absent until the sandbox's pod starts: the wall clock is a duration the
             *     sandbox is given once it runs, not one it spends waiting to be
             *     scheduled, so a sandbox that has not started has no deadline to state.
             */
            expires_at?: string | null;
            /**
             * @description Stable, content-free classification for a sandbox that did not simply
             *     finish: which bound `expired` or `ceiling_exceeded` reached, or what
             *     broke under `failed`.
             */
            failure_reason?: string | null;
            /** @description Harness driven headless inside the sandbox. */
            harness: components["schemas"]["ConversationHarness"];
            /**
             * Format: uuid
             * @description Stable sandbox ID.
             */
            id: string;
            /**
             * Format: int32
             * @description Idle ceiling in seconds, resolved at spawn.
             */
            idle_timeout_seconds: number;
            /**
             * Format: int32
             * @description Node allocatable CPU, millicores.
             */
            landed_allocatable_cpu_millicores?: number | null;
            /**
             * Format: int32
             * @description Node allocatable memory, MiB.
             */
            landed_allocatable_memory_mib?: number | null;
            /** @description Normalized capacity type: `spot`, `on-demand`, or `unknown`. */
            landed_capacity_type?: string | null;
            /** @description Cloud from the node's `providerID` prefix. */
            landed_cloud?: string | null;
            /** @description `node.kubernetes.io/instance-type`. */
            landed_instance_type?: string | null;
            /** @description Kubernetes node name, snapshotted while the pod was scheduled. */
            landed_node_name?: string | null;
            /** @description `topology.kubernetes.io/region`. */
            landed_region?: string | null;
            /** @description `topology.kubernetes.io/zone`. */
            landed_zone?: string | null;
            /** @description UTC timestamp of the last observed activity, RFC3339. */
            last_activity_at: string;
            /**
             * Format: int64
             * @description Highest event sequence persisted, or zero for a silent sandbox.
             */
            latest_event_seq: number;
            /**
             * Format: int32
             * @description Optional explicit turn budget resolved at spawn.
             */
            max_turns?: number | null;
            /** @description Whether that turn passed the mechanical autonomous-resume gates. */
            may_resume?: boolean | null;
            /**
             * Format: int32
             * @description Memory limit in MiB, snapshotted at spawn; the bound an `oom_killed`
             *     failure hit.
             */
            memory_limit_mib: number;
            /**
             * Format: int64
             * @description Reserved memory-MiB-seconds (wall duration × profile request).
             */
            memory_mib_seconds: number;
            /**
             * Format: int32
             * @description Memory request in MiB, snapshotted at spawn.
             */
            memory_request_mib: number;
            /** @description Continuation policy resolved at spawn (ADR 0047). */
            mode: components["schemas"]["SandboxRunMode"];
            /**
             * Format: int64
             * @description Messages appended but not yet acknowledged as delivered to the harness.
             */
            pending_messages: number;
            /** @description Derived watch-and-steer phase (ADR 0044). Never persisted. */
            phase: components["schemas"]["SandboxPhase"];
            /** @description Scheduling message, turn number, or failure reason. */
            phase_detail?: string | null;
            /**
             * Format: uuid
             * @description Profile the sandbox was spawned from.
             */
            profile_id: string;
            /** @description That profile's name, as it reads now. */
            profile_name: string;
            /** @description Every repository this sandbox clones and pins git egress to. */
            repositories: components["schemas"]["SandboxRepositoryView"][];
            /** @description Branch or commit started from. */
            repository_ref?: string | null;
            /**
             * @description Repository this sandbox's git egress is pinned to.
             *
             *     Absent for a research sandbox.
             */
            repository_url?: string | null;
            /** @description Gateway model ID the harness is driven with, when the spawn named one. */
            requested_model?: string | null;
            requested_reasoning_effort?: null | components["schemas"]["ReasoningEffort"];
            /**
             * @description Runner class the spawn named, when it did (ADR 0109). `null` means the
             *     profile default or a profile with no classes.
             */
            requested_runner_class?: string | null;
            /**
             * Format: uuid
             * @description Subscription account this sandbox's inference is pinned to, when the
             *     spawn named one. Absent means selection policy chooses per request.
             */
            requested_subscription_binding_id?: string | null;
            /**
             * @description Runner class the pod was sized from, when one resolved (ADR 0109).
             *
             *     Requested versus finished is the record an operator reads to decide
             *     whether a default should move; escalation (ADR 0109 item 5) amends this
             *     row as it lands. `null` means the profile's inline size applied.
             */
            runner_class?: string | null;
            /** @description Detail that accompanied [`Self::scheduling_reason`]. */
            scheduling_message?: string | null;
            /**
             * @description Substrate's last word on placing or starting the pod, while still pending.
             *
             *     Absent once the pod has started or when the substrate has said nothing
             *     (ADR 0040 C1).
             */
            scheduling_reason?: string | null;
            /**
             * Format: uuid
             * @description Server-bound spawning agent.
             */
            spawning_conversation_agent_id?: string | null;
            /**
             * Format: uuid
             * @description Server-bound spawning conversation; the workload asserts no lineage.
             */
            spawning_conversation_id?: string | null;
            /**
             * Format: uuid
             * @description Server-bound spawning run.
             */
            spawning_conversation_run_id?: string | null;
            /**
             * @description What [`Self::spend_microusd`] and [`Self::spend_ceiling_microusd`] measure.
             *
             *     Always `model_cost`: shadow model price across every billing class,
             *     including provider-subscription, provisioned-capacity, and prepaid-credit usage. Organizational
             *     cost limits are a different meter and do not terminate the sandbox.
             */
            spend_basis: components["schemas"]["SandboxSpendBasis"];
            /**
             * Format: int64
             * @description Spend ceiling in micro-US-dollars of shadow model cost, resolved at spawn.
             */
            spend_ceiling_microusd: number;
            /**
             * Format: int64
             * @description Consumed inference spend in micro-US-dollars of shadow model cost.
             *
             *     Every billing class counts. See [`Self::spend_basis`].
             */
            spend_microusd: number;
            /** @description Current lifecycle state. */
            state: components["schemas"]["SandboxState"];
            /** @description Task handed to the harness. */
            task_prompt: string;
            /**
             * @description The requester's own words for a `cancelled` stop, when they gave any
             *     (for example `review posted`). Distinguishes a run somebody killed from
             *     one whose product had already shipped.
             */
            termination_detail?: string | null;
            termination_reason?: null | components["schemas"]["SandboxTerminationReason"];
            /**
             * Format: uuid
             * @description Account that asked the sandbox to stop; `null` for a stop the gateway
             *     decided on its own (a ceiling, a revoked grant).
             */
            termination_requested_by?: string | null;
            /**
             * Format: int32
             * @description Most recently completed turn number visible in the event stream.
             */
            turns_used?: number | null;
            /**
             * Format: uuid
             * @description Owning user, whose live grants every check in the sandbox evaluates against.
             */
            user_id: string;
            /**
             * Format: int32
             * @description Wall-clock ceiling in seconds, resolved at spawn; also the lease lifetime.
             */
            wall_clock_timeout_seconds: number;
        };
        /** @description SCIM connector listing. */
        ScimConnectorListResponse: {
            /** @description Every connector, retired ones included. */
            data: components["schemas"]["ScimConnectorView"][];
        };
        /** @description A SCIM connector and the terms it authenticates on. */
        ScimConnectorView: {
            /** @description When the connector was created. */
            created_at: string;
            /** @description Whether bearer authentication is accepted. */
            enabled: boolean;
            /**
             * Format: uuid
             * @description Stable connector ID.
             */
            id: string;
            /** @description When the connector last authenticated successfully. */
            last_used_at?: string | null;
            /** @description Administrator-facing name. */
            name: string;
            /** @description Non-secret leading characters of the bearer token, for identification. */
            token_prefix: string;
        };
        /** @enum {string} */
        ScopeType: "installation" | "user" | "team" | "gateway_model" | "connected_app";
        /** @description One steering message bound for a running sandbox. */
        SendSandboxMessageRequest: {
            /** @description Ordinary input for the sandbox's next turn, at most 32 KiB. */
            body: string;
            /** @description Whether to preempt the turn in flight rather than wait for it to end. */
            interrupt?: boolean;
            /**
             * Format: uuid
             * @description Optional opaque key to recover the same receipt after a lost response.
             */
            message_id?: string | null;
        };
        /** @description One connected app a service identity reaches. */
        ServiceIdentityAppView: {
            /**
             * Format: uuid
             * @description Connected app ID.
             */
            app_id: string;
            /** @description The app's kind, as `listConnectedApps` spells it. */
            app_kind: components["schemas"]["ConnectedAppKind"];
            /** @description Whether the app is enabled. */
            enabled: boolean;
            /**
             * @description `identity` when the identity holds the app itself, `team` when its
             *     owning team is granted it. For one origin, `identity` outranks `team`
             *     wherever exactly one forge app must serve.
             */
            grant_source: components["schemas"]["GitForgeGrantSource"];
            /** @description The app's display name. */
            name: string;
            /**
             * @description For a git-forge app, the origin it serves (`scheme://host[:port]`),
             *     so a reader can see which apps compete for one origin.
             */
            origin?: string | null;
            /**
             * Format: uuid
             * @description The granting team, for a `team` grant.
             */
            team_id?: string | null;
            /** @description The granting team's name, for a `team` grant. */
            team_name?: string | null;
        };
        /** @description Service-identity listing. */
        ServiceIdentityListResponse: {
            /** @description Every service identity. */
            data: components["schemas"]["ServiceIdentityView"][];
        };
        /** @description A service identity. */
        ServiceIdentityView: {
            /**
             * @description Every connected app the identity reaches, with how it reaches each
             *     (ADR 0107 decision 5). An app held directly and inherited from the
             *     team appears once, as `identity`, because that is the grant that wins
             *     the one-per-origin choice.
             */
            apps: components["schemas"]["ServiceIdentityAppView"][];
            /** @description Display name, when one is set. */
            display_name?: string | null;
            /**
             * Format: uuid
             * @description Stable user ID, accepted everywhere a `user_id` is.
             */
            id: string;
            /** @description Whether the identity accepts new sessions. */
            is_active: boolean;
            /**
             * Format: uuid
             * @description Owning team ID, when the membership exists.
             */
            team_id?: string | null;
            /** @description Owning team's name. */
            team_name?: string | null;
            /** @description Owning team's slug. */
            team_slug?: string | null;
            /** @description Stable handle, unique across the installation. */
            username: string;
        };
        /** @description Request to enable or disable Anthropic Message Batches on one route. */
        SetAnthropicMessageBatchesRequest: {
            /** @description Target state. */
            enabled: boolean;
        };
        /** @description Request to grant or revoke one connected app for a team. */
        SetAppGrantRequest: {
            /**
             * Format: uuid
             * @description Connected app the grant concerns.
             */
            app_id: string;
            /** @description Whether the team should hold the grant. */
            granted: boolean;
        };
        /** @description Request to set a Bedrock provider's project and retention policy. */
        SetBedrockPolicyRequest: {
            /**
             * @description Bedrock project this provider routes through.
             * @example default
             */
            mantle_project_id: string;
            /** @description Maximum retention mode to accept from that project. */
            mantle_retention_policy: components["schemas"]["BedrockDataRetentionPolicy"];
        };
        /** @description Request to repair one catalog row's canonical identity. */
        SetCanonicalModelIdRequest: {
            /**
             * @description The underlying model this route serves, lowercase and matching
             *     `^[a-z0-9][a-z0-9._:-]{0,126}$`.
             * @example claude-opus-5
             */
            canonical_model_id: string;
        };
        /** @description Request to change which agent interfaces expose an app. */
        SetConnectedAppAgentAccessRequest: {
            /** @description The interfaces to expose. */
            agent_access_mode: components["schemas"]["ConnectedAppAccessMode"];
        };
        /** @description Request to set who may reach a built-in app. */
        SetConnectedAppAudienceRequest: {
            /** @description The population that may reach the app. */
            audience: components["schemas"]["ConnectedAppAudience"];
        };
        /** @description Request to replace the CA certificates an app's egress trusts. */
        SetConnectedAppCaTrustRequest: {
            /**
             * @description PEM bundle of one or more CA certificates, or blank to clear it.
             *
             *     Blank restores the platform trust store alone, which is the same state
             *     `deleteConnectedAppCaTrust` leaves and what an app that has never been
             *     configured holds.
             */
            ca_trust_bundle: string;
        };
        /** @description Request to enable or disable an app. */
        SetConnectedAppEnabledRequest: {
            /** @description Whether the app may be invoked. */
            enabled: boolean;
        };
        /** @description Request to replace an app's model policy. */
        SetConnectedAppModelPolicyRequest: {
            direct_endpoint_resolution?: null | components["schemas"]["DirectEndpointResolution"];
            /** @description Whether every granted route may invoke the app, or only listed ones. */
            mode: components["schemas"]["ModelAccessMode"];
            /** @description The allowed routes. Must be empty when the mode is `all`. */
            model_ids?: string[];
        };
        /** @description Request to rename an app in place. */
        SetConnectedAppNameRequest: {
            /** @description The name to carry from now on, 1 to 120 characters. */
            name: string;
        };
        /** @description Request to replace the OAuth scopes used by future Connect flows. */
        SetConnectedAppOauthScopesRequest: {
            /** @description Space-separated scopes, or blank to omit the `scope` parameter. */
            scopes: string;
        };
        /** @description Replacement route for automatic conversation title generation. */
        SetConversationTitleTaskRequest: {
            /** @description Whether automatic title generation is enabled. */
            enabled: boolean;
            /**
             * Format: uuid
             * @description Primary gateway-model catalog row. Required when enabling.
             */
            model_id?: string | null;
        };
        /** @description Request to set one multiplier. */
        SetCostMultiplierRequest: {
            /**
             * @description Pinned geography for a residency multiplier, or `*` for any pinned geography. Must
             *     be omitted for batch and negotiated discounts.
             */
            geo?: string | null;
            /** @description What the multiplier represents. */
            kind: string;
            /**
             * @description The factor as a decimal string with at most six decimal places, e.g. `"1.1"` for a
             *     ten percent uplift or `"0.85"` for a fifteen percent discount.
             *
             *     A decimal string rather than a number for the reason rates are: binary floating
             *     point cannot represent every value exactly, and a silently rounded multiplier bills
             *     real money incorrectly.
             */
            multiplier: string;
        };
        /**
         * @description Request to change the administrator-facing model name without changing its
         *     routing identity.
         */
        SetDisplayNameRequest: {
            /**
             * @description Human name shown in admin surfaces and gateway-discovered model pickers.
             *     Add serving context when sibling routes coexist, for example
             *     `Claude Opus 5 · Bedrock US CRIS`.
             */
            display_name: string;
        };
        /** @description Request to change a model's or provider's enabled state. */
        SetEnabledRequest: {
            /** @description Target state. */
            enabled: boolean;
        };
        /** @description Request to choose the installation a forge mints tokens for. */
        SetGitForgeGithubAppInstallationRequest: {
            /**
             * Format: int64
             * @description Installation identifier, as `listGitForgeGithubAppInstallations` reports it.
             */
            installation_id: number;
        };
        /** @description Request to offer a provider at sign-in, or withdraw it. */
        SetIdentityProviderEnabledRequest: {
            /** @description Whether the provider is offered at sign-in. */
            enabled: boolean;
        };
        /** @description Request to set who a provider may onboard. */
        SetIdentityProviderProvisioningRequest: {
            /** @description Email domains allowed to create accounts; empty means unrestricted. */
            allowed_email_domains?: string[];
            /** @description Whether an unmatched verified identity may create an account at sign-in. */
            jit_enabled: boolean;
        };
        /** @description Request to repair a model's limits. */
        SetLimitsRequest: {
            /**
             * Format: int32
             * @description Context window in tokens, from the publisher's documentation.
             */
            context_window: number;
            /**
             * Format: int32
             * @description Input token count above which estimates apply the long-context surcharge.
             *     Omit or null to clear it.
             */
            long_context_input_threshold?: number | null;
            /**
             * Format: int32
             * @description Maximum output tokens, which cannot exceed the context window.
             */
            max_output_tokens: number;
        };
        /** @description Request to add or remove one app assignment. */
        SetMcpEndpointAppRequest: {
            /** @description Whether the endpoint exposes this app. */
            assigned: boolean;
        };
        /** @description Request to let clients connect or stop them. */
        SetMcpEndpointEnabledRequest: {
            /** @description Whether clients may connect. */
            enabled: boolean;
        };
        /** @description Request to rename an endpoint. */
        SetMcpEndpointNameRequest: {
            /** @description Administrator-facing name, 1 to 120 characters. */
            name: string;
        };
        /** @description Request to attach or detach one profile on one endpoint. */
        SetMcpEndpointSandboxProfileRequest: {
            /** @description Whether this endpoint's clients may select this profile. */
            attached: boolean;
        };
        /** @description Request to move an endpoint between trust tiers. */
        SetMcpExecutionModeRequest: {
            /** @description The tier to move to. */
            execution_mode: components["schemas"]["McpExecutionMode"];
        };
        /** @description Request to add or update a team membership. */
        SetMembershipRequest: {
            /** @description Membership role, or the explicit `remove` action. */
            membership: components["schemas"]["TeamMembershipChange"];
            /**
             * Format: uuid
             * @description User to place in the team.
             */
            user_id: string;
        };
        /** @description Request to grant or revoke one model for a team. */
        SetModelGrantRequest: {
            /** @description Whether the team should hold the grant. */
            granted: boolean;
            /**
             * Format: uuid
             * @description Catalog model the grant concerns.
             */
            model_id: string;
        };
        /** @description Request to set whether personal credentials may be used. */
        SetPersonalCredentialPolicyRequest: {
            /** @description The policy to apply. */
            personal_credential_policy: components["schemas"]["PersonalCredentialPolicy"];
        };
        /** @description Request to grant or revoke one model for one person. */
        SetPersonModelGrantRequest: {
            /** @description Whether the person should hold the grant. */
            granted: boolean;
        };
        /** @description Request to set an account's local password. */
        SetPersonPasswordRequest: {
            /**
             * Format: password
             * @description The new password.
             */
            password: string;
        };
        /**
         * @description Request to set a model's rates, now or at a future instant.
         *
         *     Rates are decimal strings for the same reason they are on model creation:
         *     binary floating point cannot represent every rate exactly, and a silently
         *     rounded price bills real money incorrectly. Omitting a field sets that rate
         *     to null; this replaces the rate set rather than patching individual fields,
         *     so read the current values first and send them back unchanged.
         */
        SetPricingRequest: {
            /** @description One-hour cache-write rate. Omit to leave those writes unpriced. */
            cache_write_1h_price_per_million?: string | null;
            /** @description Five-minute cache-write rate. Omit to leave those writes unpriced. */
            cache_write_5m_price_per_million?: string | null;
            /** @description Cache-read rate. Omit to bill cached input at the input rate. */
            cached_input_price_per_million?: string | null;
            /**
             * @description RFC3339 instant the rates start applying at, or null to apply them now.
             *
             *     This field decides whether the edit touches history, so it is not a
             *     formatting preference. Null means the rates are true as of now, and any
             *     rate that was never configured is treated as having always been this
             *     value: it is written backward over the intervals missing it and their
             *     events are requeued for reconciliation. A date means the opposite claim
             *     — that the rate starts being true then — so nothing before it is
             *     touched and nothing is requeued. Supply a date only for a change the
             *     provider has announced ahead of time; leave it null to repair rates that
             *     should have been recorded all along.
             *
             *     Must be strictly in the future. A past instant is a correction to
             *     history, which is a different operation and is rejected here.
             */
            effective_from?: string | null;
            /** @description Fast-tier one-hour cache-write rate. */
            fast_cache_write_1h_price_per_million?: string | null;
            /** @description Fast-tier five-minute cache-write rate. */
            fast_cache_write_5m_price_per_million?: string | null;
            /** @description Fast-tier cache-read rate. Omit to bill fast cached input at the fast input rate. */
            fast_cached_input_price_per_million?: string | null;
            /**
             * @description Premium-tier input rate, for a model whose provider offers one.
             *
             *     Anthropic spells this tier `fast` and `OpenAI` spells it `priority`; a model has
             *     one premium rate set for whichever its provider offers, so these fields carry
             *     either.
             *
             *     The fast rates are their own all-or-nothing pair and their own fallback base: a
             *     fast cache rate needs a fast input rate, and no fast rate ever falls back to a
             *     standard one. Leaving these out prices nothing at standard — a request the
             *     provider served fast is recorded `pending_rate` until a fast rate exists, because
             *     billing it at the standard rate would understate it and settle as final.
             *
             *     This write replaces every rate, so a submission that omits these clears them.
             */
            fast_input_price_per_million?: string | null;
            /** @description Fast-tier output rate in US dollars per million tokens. */
            fast_output_price_per_million?: string | null;
            /** @description Flex-tier one-hour cache-write rate. */
            flex_cache_write_1h_price_per_million?: string | null;
            /** @description Flex-tier five-minute cache-write rate. */
            flex_cache_write_5m_price_per_million?: string | null;
            /** @description Flex-tier cache-read rate. Omit to bill flex cached input at the flex input rate. */
            flex_cached_input_price_per_million?: string | null;
            /**
             * @description Discount-tier input rate, for a model whose provider offers one.
             *
             *     `OpenAI` spells this tier `flex`. It trades latency for roughly half the standard
             *     per-token rate on the models that offer it.
             *
             *     The flex rates are their own all-or-nothing pair and their own fallback base: a
             *     flex cache rate needs a flex input rate, and no flex rate ever falls back to a
             *     standard one. Leaving these out prices nothing at standard — a request the
             *     provider served flex is recorded `pending_rate` until a flex rate exists, because
             *     billing it at the standard rate would overstate it and settle as final.
             *
             *     This write replaces every rate, so a submission that omits these clears them.
             */
            flex_input_price_per_million?: string | null;
            /** @description Flex-tier output rate in US dollars per million tokens. */
            flex_output_price_per_million?: string | null;
            /**
             * @description Input rate in US dollars per million tokens, e.g. `"1.25"`.
             *
             *     Input and output rates are all-or-nothing: supply both or neither. Any
             *     cache rate requires an input rate to fall back to.
             */
            input_price_per_million?: string | null;
            /** @description Output rate in US dollars per million tokens. */
            output_price_per_million?: string | null;
            /** @description Ultrafast-tier one-hour cache-write rate. */
            ultrafast_cache_write_1h_price_per_million?: string | null;
            /** @description Ultrafast-tier five-minute cache-write rate. */
            ultrafast_cache_write_5m_price_per_million?: string | null;
            /** @description Ultrafast-tier cache-read rate. Omit to use the Ultrafast input rate. */
            ultrafast_cached_input_price_per_million?: string | null;
            /**
             * @description Contract input rate for `OpenAI` Ultrafast processing.
             *
             *     The Ultrafast rates are their own all-or-nothing pair and their own fallback
             *     base. An Ultrafast cache rate needs an Ultrafast input rate, and no Ultrafast
             *     rate falls back to Fast or Standard. A served request with no base rate stays
             *     `pending_rate` because `OpenAI` publishes no public Ultrafast price.
             *
             *     This write replaces every rate, so a submission that omits these clears them.
             */
            ultrafast_input_price_per_million?: string | null;
            /** @description Ultrafast-tier output rate in US dollars per million tokens. */
            ultrafast_output_price_per_million?: string | null;
        };
        /** @description Request to set the installation API key a provider authenticates with. */
        SetProviderApiKeyRequest: {
            /** @description The key to store. Sent once; no read returns it afterwards. */
            api_key: string;
        };
        /** @description Request to rename a provider. */
        SetProviderNameRequest: {
            /** @description Administrator-facing name, 1 to 120 characters. */
            name: string;
        };
        /** @description Desired metered-fallback preference for one caller-owned subscription. */
        SetProviderSubscriptionFallbackRequest: {
            /** @description Whether metered credentials may serve after this subscription is unavailable. */
            enabled: boolean;
        };
        /** @description New user-facing label for one caller-owned subscription. */
        SetProviderSubscriptionLabelRequest: {
            /** @description Label after trimming; must contain 1 to 120 Unicode characters. */
            label: string;
        };
        /** @description Request to change which `OpenAI` API an OpenAI-compatible provider serves. */
        SetProviderWireApiRequest: {
            /** @description Southbound API the provider implements. */
            openai_compatible_api: components["schemas"]["OpenAiCompatibleApi"];
        };
        /** @description Request to change how one route binds to provider-provisioned capacity. */
        SetProvisionedCapacityRequest: {
            /** @description Provider-specific capacity behavior for this route. */
            mode: components["schemas"]["ProvisionedCapacityMode"];
        };
        /** @description Request to change which reasoning effort levels one model's upstream accepts. */
        SetReasoningEffortsRequest: {
            /**
             * @description Every accepted token, any order. `null` restores "unstated" — the
             *     upstream decides; an empty array records that the model takes no
             *     effort control.
             */
            supported_reasoning_efforts?: components["schemas"]["ReasoningEffort"][] | null;
        };
        /** @description Request to open a profile for spawning, or close it. */
        SetSandboxProfileEnabledRequest: {
            /** @description Whether sandboxes may be spawned from this profile. */
            enabled: boolean;
        };
        /** @description Request to retire a SCIM connector. */
        SetScimConnectorEnabledRequest: {
            /** @description Only `false` is accepted; see the operation description. */
            enabled: boolean;
        };
        /** @description Request to grant or revoke a service identity's own connected app. */
        SetServiceIdentityAppGrantRequest: {
            /**
             * @description `true` grants the app to the identity directly; `false` removes the
             *     direct grant. Team-inherited access is untouched either way.
             */
            granted: boolean;
        };
        /** @description Request to enable or disable a service identity. */
        SetServiceIdentityEnabledRequest: {
            /**
             * @description `false` is the kill switch: sessions revoke and live sandboxes are
             *     asked to stop in the same transaction.
             */
            enabled: boolean;
        };
        /** @description Request to move a service identity to a new owning team. */
        SetServiceIdentityTeamRequest: {
            /**
             * Format: uuid
             * @description New owning team. The old membership is replaced in the same
             *     transaction — an identity holds exactly one.
             */
            team_id: string;
        };
        /** @description Request to set whether a provider admits per-user subscription credentials. */
        SetSubscriptionPolicyRequest: {
            /**
             * @description Whether a subscription may only be used by a request an attributed CLI
             *     session vouched for.
             */
            cli_only: boolean;
            /**
             * @description Starting metered-fallback preference for subscriptions attached after
             *     this call. Existing bindings keep the preference their owner has.
             */
            fallback_default: boolean;
            /**
             * Format: int32
             * @description Remaining percentage to reserve before reserve-aware borrowing begins.
             *     Inclusive range: 0 through 100. Omit to preserve the current value.
             */
            headroom_percent?: number | null;
            /** @description Whether users may attach their own subscription. */
            policy: components["schemas"]["SubscriptionCredentialPolicy"];
            /**
             * @description Whether known reset deadlines should outrank generic least-used order.
             *     Omit to preserve the current value.
             */
            reset_priority_enabled?: boolean | null;
            /**
             * @description Whether a gateway-hosted sandbox's session is excluded from subscription
             *     resolution.
             *
             *     Separate from `cli_only` because a sandbox session satisfies that
             *     control technically: whether a detached headless run honors its *intent*
             *     is a terms-of-service judgment only an administrator can make, and the
             *     request that this refuses is recorded as `subscription_sandbox_excluded`
             *     rather than as a CLI-only denial it actually passed.
             */
            sandbox_excluded?: boolean;
            shared_routing_policy?: null | components["schemas"]["SubscriptionSharedRoutingPolicy"];
            /**
             * @description Whether owners may lend their bindings to teams as overflow
             *     (ADR 0037). Omit to leave the current setting alone — unlike the two
             *     fields above, this one is not replaced by silence, because switching it
             *     off ends every borrowed session in flight and no caller should do that
             *     by forgetting a field.
             */
            sharing_enabled?: boolean | null;
        };
        /** @description Request to enable or disable a team. */
        SetTeamEnabledRequest: {
            /** @description Whether the team's grants are active. */
            enabled: boolean;
        };
        /** @description Request to rename a team. */
        SetTeamNameRequest: {
            /**
             * @description New human-readable name, unique case-insensitively.
             * @example Platform Engineering
             */
            name: string;
        };
        /** @description A shared app and the revision created with it. */
        SharedAppCreatedView: {
            /**
             * Format: uuid
             * @description Stable shared-app ID.
             */
            id: string;
            /**
             * Format: int32
             * @description Number of the revision created, always 1.
             */
            revision: number;
            /**
             * Format: uuid
             * @description Stable ID of that revision.
             */
            revision_id: string;
            /** @description Slug the app is addressed by, derived when one was not supplied. */
            slug: string;
            /** @description Lifecycle state, always `draft`. */
            status: components["schemas"]["SharedAppStatus"];
        };
        /** @description A shared app with the manifest its current revision pins. */
        SharedAppDetailView: {
            /**
             * Format: int32
             * @description Size of that revision's bundle in bytes, absent when it carries none.
             */
            bundle_bytes?: number | null;
            /** @description Creation time. */
            created_at: string;
            /**
             * Format: int32
             * @description Number of the revision the app currently serves.
             */
            current_revision?: number | null;
            /**
             * @description Teams the app is published to, for callers entitled to see its audience.
             *
             *     `None` and `Some([])` are different answers and neither may be collapsed
             *     into the other: `None` means this caller does not get to see the
             *     audience, `Some([])` means the audience is empty. A granted viewer reads
             *     this app legitimately but must not learn which *other* teams hold it, so
             *     they get `None`; the author and an administrator get the list.
             */
            granted_teams?: components["schemas"]["SharedAppTeamGrantView"][] | null;
            /**
             * Format: uuid
             * @description Stable shared-app ID.
             */
            id: string;
            manifest?: null | components["schemas"]["SharedAppManifest"];
            /**
             * Format: int32
             * @description Number of the revision `manifest` was read from.
             *
             *     The app metadata and the manifest are two reads, so this is the field
             *     that says which revision the manifest below actually is.
             *     `current_revision` says what the app row claimed a moment earlier, and a
             *     republish between the two makes them differ.
             */
            manifest_revision?: number | null;
            /** @description Human-readable name. */
            name: string;
            /**
             * Format: uuid
             * @description Authoring owner.
             */
            owner_user_id: string;
            /** @description Slug, unique within the owner. */
            slug: string;
            /** @description Lifecycle state. */
            status: components["schemas"]["SharedAppStatus"];
            /** @description Last metadata change. */
            updated_at: string;
        };
        /** @description Shared-app listing. */
        SharedAppListResponse: {
            /** @description Every shared app this caller reaches. */
            data: components["schemas"]["SharedAppView"][];
            /** @description Whether more apps were reachable than the cap returned. */
            truncated: boolean;
        };
        /**
         * @description The security object of a shared app: what it may call, and as whom.
         *
         *     Unknown members are refused rather than ignored. A manifest is a statement
         *     about what an app is permitted to do, and a gateway that quietly dropped a
         *     member it did not recognize would be running an app under a narrower
         *     contract than its author wrote and believes is in force.
         */
        SharedAppManifest: {
            /** @description Every `{connected app, operations}` pair this revision may call. */
            bindings: components["schemas"]["ManifestBinding"][];
            /** @description Inputs the app takes, for the shell to render and the bundle to read. */
            parameters?: components["schemas"]["ManifestParameter"][];
            /** @description Display title the shell renders above the bundle. */
            title: string;
        };
        /** @description One appended revision. */
        SharedAppRevisionView: {
            /**
             * Format: int32
             * @description Monotonic per-app revision number.
             */
            revision: number;
            /**
             * Format: uuid
             * @description Stable revision ID.
             */
            revision_id: string;
            /**
             * Format: uuid
             * @description Shared app the revision belongs to.
             */
            shared_app_id: string;
        };
        /**
         * @description Lifecycle state of a user-authored, team-shared app (ADR 0036).
         * @enum {string}
         */
        SharedAppStatus: "draft" | "published" | "disabled";
        /** @description One team a shared app is published to. */
        SharedAppTeamGrantView: {
            /**
             * @description Whether the team's inherited access is active. A grant on a disabled
             *     team is still recorded here, and still reaches nobody.
             */
            enabled: boolean;
            /** @description Team's human-readable name. */
            name: string;
            /** @description Team's stable slug. */
            slug: string;
            /**
             * Format: uuid
             * @description Team holding the grant.
             */
            team_id: string;
        };
        /** @description A shared app's metadata, without its manifest or its authored bytes. */
        SharedAppView: {
            /** @description Creation time. */
            created_at: string;
            /**
             * Format: int32
             * @description Number of the revision the app currently serves.
             */
            current_revision?: number | null;
            /**
             * Format: uuid
             * @description Stable shared-app ID.
             */
            id: string;
            /** @description Human-readable name. */
            name: string;
            /**
             * Format: uuid
             * @description Authoring owner.
             */
            owner_user_id: string;
            /** @description Slug, unique within the owner. */
            slug: string;
            /** @description Lifecycle state. */
            status: components["schemas"]["SharedAppStatus"];
            /** @description Last metadata change. */
            updated_at: string;
        };
        /** @description Owner reserve on a team share. Omitted fields keep the uncapped grant. */
        ShareProviderSubscriptionRequest: {
            /** @description Optional per-window caps keyed like the account-page snapshot (`5h`, `7d`, `7d-oi`). */
            window_caps?: components["schemas"]["ShareWindowCapRequest"][] | null;
        };
        /** @description One usage-window reserve in a share request. */
        ShareWindowCapRequest: {
            /**
             * Format: int32
             * @description Stop borrowers when this window reaches this used-percent.
             */
            cap_percent: number;
            /** @description Snapshot window key, e.g. `5h` or `7d-oi`. */
            window_key: string;
        };
        /**
         * @description How a new account signs in.
         * @enum {string}
         */
        SignInMode: "password" | "sso";
        /** @description One live cooldown on a binding. */
        SubscriptionCooldownView: {
            /**
             * @description Provider quota family the cooldown gates, e.g. Anthropic `oi`. Null
             *     gates every family, which is the only value that stops the binding
             *     outright.
             */
            model_scope?: string | null;
            /**
             * Format: int64
             * @description When the cooldown lifts, as Unix seconds.
             */
            until_unix_seconds: number;
        };
        /**
         * @description Whether a model provider may use credentials from a user's consumer subscription.
         * @enum {string}
         */
        SubscriptionCredentialPolicy: "disabled" | "subscription_preferred";
        /**
         * @description Provider-reported availability of one user's subscription credential.
         * @enum {string}
         */
        SubscriptionLimitState: "available" | "cooling_down" | "exhausted_reauth";
        /**
         * @description How team-shared subscriptions participate in unpinned selection.
         * @enum {string}
         */
        SubscriptionSharedRoutingPolicy: "strict_overflow" | "reserve_aware" | "pooled";
        /**
         * @description One provider quota window as the gateway last observed it.
         *
         *     Normalized from the provider's own headers, so the vocabulary is the
         *     provider's: no field here is a gateway invention, and an absent one means
         *     the provider did not advertise it rather than zero.
         */
        SubscriptionUsageWindowView: {
            /** @description Provider window key, e.g. `5h`, `7d`, `7d-opus`, `primary`. */
            key: string;
            /** @description Human-readable label for the window, e.g. `Session (5h)`. */
            label: string;
            /**
             * @description Quota family this window meters; null means it meters the whole
             *     binding.
             */
            model_scope?: string | null;
            /**
             * Format: int64
             * @description When this particular window was observed, as Unix seconds. Not always
             *     the snapshot time: windows advertised only by some models are merged in
             *     as they are seen.
             */
            observed_at_unix_seconds?: number | null;
            /**
             * Format: int64
             * @description When the window resets, as Unix seconds, when the provider advertised
             *     it. A deadline in the past means the window has already reset and this
             *     snapshot predates it.
             */
            resets_at_unix_seconds?: number | null;
            /**
             * @description The provider's own verdict for this window (`allowed`,
             *     `allowed_warning`, `rejected`) when it advertised one — the
             *     authoritative signal for which window refused a request.
             */
            status?: string | null;
            /**
             * Format: double
             * @description Consumed share of the window, 0 to 100.
             */
            used_percent: number;
            /**
             * Format: int64
             * @description Window length in minutes, when advertised.
             */
            window_minutes?: number | null;
        };
        /** @description Team listing. */
        TeamListResponse: {
            /** @description Every team. */
            data: components["schemas"]["TeamView"][];
        };
        /**
         * @description Resulting membership requested for a user.
         * @enum {string}
         */
        TeamMembershipChange: "member" | "manager" | "remove";
        /** @description Membership listing for one team. */
        TeamMembershipListResponse: {
            /** @description Every current membership in the team. */
            data: components["schemas"]["TeamMembershipView"][];
        };
        /**
         * @description Role held by a person within one team.
         * @enum {string}
         */
        TeamMembershipRole: "member" | "manager";
        /** @description One user's membership in a team. */
        TeamMembershipView: {
            /** @description Role the user holds in this team. */
            role: components["schemas"]["TeamMembershipRole"];
            /**
             * Format: uuid
             * @description Assigned user.
             */
            user_id: string;
        };
        /** @description Traffic attributed to one team at request time. */
        TeamUsageView: {
            /**
             * Format: int64
             * @description Governed connected-app HTTP requests.
             */
            app_requests: number;
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *     Same contract as the by-user rollup: billed rows only, floored,
             *     negative when write premiums exceeded read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Estimated metered cost, in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Model requests attributed to this team.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Input tokens across those requests.
             */
            input_tokens: number;
            /**
             * Format: int64
             * @description Output tokens across those requests.
             */
            output_tokens: number;
            /** Format: int64 */
            pending_rate_requests: number;
            /**
             * Format: int64
             * @description How many of those requests carry a final price.
             */
            priced_requests: number;
            /** Format: int64 */
            provisional_requests: number;
            /** @description Current team name, or the slug when the team has been deleted. */
            team_name: string;
            /** @description Team slug recorded with each request. */
            team_slug: string;
            /**
             * Format: int64
             * @description MCP tool calls authorized by this team's app grants.
             */
            tool_calls: number;
            /** Format: int64 */
            unpriceable_requests: number;
        };
        /** @description A team. */
        TeamView: {
            /**
             * Format: int64
             * @description Current connected-app-grant count.
             */
            app_count: number;
            /**
             * @description Whether the team's grants are currently active.
             *
             *     Disabling a team suspends every grant it carries without deleting them.
             */
            enabled: boolean;
            /**
             * Format: uuid
             * @description Stable team ID.
             */
            id: string;
            /**
             * Format: int64
             * @description Current person-member count. Service identities owned by the team are
             *     not counted; list them with `listServiceIdentities`.
             */
            member_count: number;
            /**
             * Format: int64
             * @description Current model-grant count.
             */
            model_count: number;
            /** @description Human-readable name. */
            name: string;
            /** @description Stable slug. */
            slug: string;
        };
        /** @description One certificate in an app's trust bundle. */
        TrustedCertificateView: {
            /**
             * @description Whether the certificate carries the CA basic constraint.
             *
             *     False is legitimate — pinning a self-managed host's own self-signed
             *     certificate is a real configuration — so this is reported rather than
             *     refused, and the console says which kind is in use.
             */
            is_certificate_authority: boolean;
            /** @description Distinguished name of the issuer; equal to the subject when self-signed. */
            issuer: string;
            /** @description End of the validity window, RFC 3339. This is the date to watch. */
            not_after?: string | null;
            /** @description Start of the validity window, RFC 3339. */
            not_before?: string | null;
            /** @description Distinguished name of the subject. */
            subject: string;
        };
        /**
         * @description Egress mode of one profile tunnel domain (issue #1132, ADR 0072).
         * @enum {string}
         */
        TunnelDomainMode: "blind" | "intercepted";
        /**
         * @description One tunnel domain in a profile write.
         *
         *     A bare string is the original contract and means a blind relay; the object
         *     form names the egress mode explicitly, so the operator's grant states its
         *     own observability level.
         */
        TunnelDomainRequest: string | {
            /** @description Exact lowercase hostname. */
            domain: string;
            /** @description Egress mode; omitting it means blind. */
            mode?: components["schemas"]["TunnelDomainMode"];
        };
        /** @description One tunnel domain with its egress mode, as the read shape reports it. */
        TunnelDomainView: {
            /** @description Exact hostname. */
            domain: string;
            /** @description Egress mode. */
            mode: components["schemas"]["TunnelDomainMode"];
        };
        UnpriceableCauseView: {
            error_code?: string | null;
            /** Format: int64 */
            event_count: number;
            gateway_id: string;
            /** Format: uuid */
            model_id?: string | null;
            outcome: string;
            provider_name: string;
        };
        /** @description Installation-wide cost-control settings. */
        UpdateCostControlSettingsRequest: {
            /**
             * @description IANA time-zone name `PostgreSQL` knows, such as `America/New_York`. Every
             *     cost window opens and closes by this zone's calendar.
             */
            time_zone: string;
        };
        /** @description The two fields a cap can change after creation. */
        UpdateCostLimitRequest: {
            /** @description Whether the cap is enforced from now on. */
            enabled: boolean;
            /**
             * Format: int64
             * @description The new cap, in micro-US-dollars: 1 000 000 to the dollar. `0` denies
             *     the scope's metered inference outright.
             */
            limit_microusd: number;
        };
        /** @description Request to replace the installation-wide failover default. */
        UpdateFailoverRoutingRequest: {
            /** @description Whether managed models derive a waterfall automatically. */
            automatic: boolean;
            /** @description Whether upstream throttling triggers failover on derived waterfalls. */
            trigger_on_rate_limit: boolean;
        };
        /** @description A full replacement of one engine instance's mutable state. */
        UpdateGuardrailEngineRequest: {
            config?: Record<string, never>;
            display_name: string;
            enabled: boolean;
            engine_key: string;
            kind: components["schemas"]["EngineKind"];
        };
        UpdateGuardrailPolicyRequest: {
            enabled: boolean;
            engine_ids: string[];
            failure_stance: components["schemas"]["FailureStance"];
            mode: components["schemas"]["Mode"];
            name: string;
        };
        /** @description Request to update an account. */
        UpdatePersonRequest: {
            /** @description Display name, or null to leave the account without one. */
            display_name?: string | null;
            /**
             * @description Email address. Send with `username` and `display_name` to replace the
             *     account's identity, or omit all three to change only role and state.
             */
            email?: string | null;
            /** @description Whether the account can authenticate. */
            is_active: boolean;
            /** @description Installation role. */
            role: components["schemas"]["UserRole"];
            /** @description Local username, or null for an account that signs in federated. */
            username?: string | null;
        };
        /** @description The two fields a policy can change after creation. */
        UpdateRateLimitRequest: {
            /** @description Whether the policy is enforced from now on. */
            enabled: boolean;
            /**
             * Format: int64
             * @description The new cap, in the unit the policy's dimension names. `0` denies every
             *     request in scope on that dimension.
             */
            limit_value: number;
        };
        /** @description Request to replace a sandbox profile's specification. */
        UpdateSandboxProfileRequest: {
            /** @description Connector-terminated connections this profile may use. */
            connector_connection_ids?: string[];
            /**
             * Format: int32
             * @description CPU ceiling in millicores, at least the request.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * @description The class a spawn gets when it names none; must be listed in
             *     `runner_classes`. Required exactly when that list is non-empty.
             */
            default_runner_class?: string | null;
            /** @description Egress bundles this profile references, replacing the stored set. */
            egress_bundles?: components["schemas"]["EgressBundleReferenceRequest"][];
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: int32
             * @description Seconds of inactivity while the client remains in the foreground. Omit to
             *     keep the ordinary idle timer.
             */
            foreground_idle_timeout_seconds?: number | null;
            /**
             * Format: int32
             * @description Seconds of inactivity before the sandbox is stopped, 60 to 86400.
             */
            idle_timeout_seconds: number;
            /** @description Digest-pinned workload image, ending `@sha256:` and 64 hex characters. */
            image: string;
            /**
             * Format: int32
             * @description Pod incarnations one sandbox may consume, 1 to 16. Defaults to 3;
             *     1 disables reincarnation after infrastructure loss (ADR 0068 part 3).
             */
            max_pod_incarnations?: number;
            /**
             * Format: int32
             * @description Memory ceiling in MiB, at least the request.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /** @description Node placement selector object; omit for no constraint. */
            node_selector?: unknown;
            /** @description Whether a spawn may declare a pinned repository. Off by default. */
            permits_repositories?: boolean;
            /** @description Harnesses this profile permits, replacing the stored set. */
            permitted_harnesses: components["schemas"]["ConversationHarness"][];
            /**
             * @description Gateway model IDs this profile permits, replacing the stored set.
             *     Empty — the default — declares no model restriction.
             */
            permitted_model_routes?: string[];
            /**
             * @description Annotations rendered onto this profile's pods; omit for none.
             *
             *     A JSON object of string keys and string values. This is where a
             *     provisioner's "do not disrupt this node" annotation goes: a sandbox pod
             *     carries `restartPolicy: Never`, so a node consolidated out from under a
             *     running sandbox ends that run for good.
             */
            pod_annotations?: unknown;
            /** @description How the workload is started. Defaults to `first_party`. */
            profile_runtime?: components["schemas"]["SandboxProfileRuntime"];
            /**
             * @description Runner classes this profile permits, replacing the stored set (ADR 0109).
             *     Empty keeps the profile on its own inline size.
             */
            runner_classes?: string[];
            /** @description Hardened runtime class the pod runs under. */
            runtime_class: string;
            /**
             * Format: int64
             * @description Per-sandbox ceiling in micro-US-dollars of shadow model cost (all billing classes).
             */
            spend_ceiling_microusd: number;
            /**
             * @description Taints this profile's pods tolerate; omit to tolerate none.
             *
             *     A JSON array of Kubernetes tolerations, each naming the taint `key` it
             *     tolerates and an `operator` of `Exists` or `Equal`. A dedicated node
             *     pool for a hardened runtime is normally tainted, so a profile with no
             *     toleration is one the scheduler will never place there.
             */
            tolerations?: unknown;
            /**
             * @description Exact-match tunnel domains, replacing the stored set — bare strings
             *     (blind) or `{domain, mode}` objects.
             */
            tunnel_domains?: components["schemas"]["TunnelDomainRequest"][];
            /**
             * Format: int32
             * @description Total seconds a sandbox may live, at least the idle timeout.
             */
            wall_clock_timeout_seconds: number;
            /** @description Repository reference whose warm layer to refresh. Omit for cold starts. */
            warm_image_reference?: string | null;
            /**
             * @description Schedule for refreshing the warm image. Required with the reference, and
             *     refused without it.
             */
            warm_image_schedule?: string | null;
        };
        /**
         * @description Request to replace a runner class's specification. Carries no name: a
         *     rename would silently move every profile that lists the old one.
         */
        UpdateSandboxRunnerClassRequest: {
            /**
             * Format: int32
             * @description CPU ceiling in millicores, at least the request.
             */
            cpu_limit_millicores: number;
            /**
             * Format: int32
             * @description Guaranteed CPU in millicores.
             */
            cpu_request_millicores: number;
            /**
             * Format: int32
             * @description Ephemeral storage ceiling in MiB.
             */
            ephemeral_storage_limit_mib: number;
            /**
             * Format: int32
             * @description Memory ceiling in MiB, at least the request.
             */
            memory_limit_mib: number;
            /**
             * Format: int32
             * @description Guaranteed memory in MiB.
             */
            memory_request_mib: number;
            /** @description Node selector object; omit for no placement constraint of its own. */
            node_selector?: unknown;
            /**
             * Format: int32
             * @description Smallest-to-largest order, 0 to 1,000,000; unique.
             */
            rank: number;
            /**
             * Format: int32
             * @description Optional wall-clock default in seconds, 60 to 604800.
             */
            wall_clock_timeout_seconds?: number | null;
        };
        /** @description Every usage rollup, over the same set of recorded events. */
        UsageResponse: {
            /** @description By connected app. */
            by_app: components["schemas"]["AppUsageView"][];
            /** @description By authenticating client. */
            by_client: components["schemas"]["ClientUsageView"][];
            /** @description By execution kind: workstation `command` vs agent `sandbox`. */
            by_execution_kind: components["schemas"]["ExecutionKindUsageView"][];
            /** @description By catalog model. */
            by_model: components["schemas"]["ModelUsageView"][];
            /** @description By sandbox that performed the inference. */
            by_sandbox: components["schemas"]["SandboxUsageView"][];
            /** @description By team attributed at request time. */
            by_team: components["schemas"]["TeamUsageView"][];
            /** @description By account. */
            by_user: components["schemas"]["UserUsageView"][];
            /** @description General grouped inference rows. Present only when `group_by` is requested. */
            grouped?: components["schemas"]["GroupedInferenceUsageView"][] | null;
            /** @description Whether additional grouped rows matched but were omitted by the hard cap. */
            grouped_truncated?: boolean | null;
            /**
             * @description Whether these totals cover the installation or only the caller.
             *
             *     `installation` for an administrator, `self` for everyone else. Stated
             *     rather than left to inference: the two shapes are identical, and a
             *     caller that reads a scoped total as installation-wide silently
             *     under-reports.
             */
            scope: string;
            summary?: null | components["schemas"]["UsageSummaryView"];
        };
        /**
         * @description Installation- or self-scoped totals for one usage window.
         *
         *     Present on unfiltered and window-only reads. Dimension filters omit it
         *     rather than silently reporting a wider total.
         */
        UsageSummaryView: {
            /**
             * Format: int64
             * @description Governed HTTP request body bytes.
             */
            app_request_bytes: number;
            /**
             * Format: int64
             * @description Governed connected-app HTTP requests.
             */
            app_requests: number;
            /**
             * Format: int64
             * @description Governed HTTP response body bytes.
             */
            app_response_bytes: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Cost drawn from prepaid usage credits, in micro-US-dollars.
             */
            credits_cost_microusd: number;
            /**
             * Format: int64
             * @description Estimated metered cost, in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Model requests in this scope and window.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Known input tokens.
             */
            input_tokens: number;
            /**
             * Format: int64
             * @description Known output tokens.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Requests awaiting one or more rate dimensions.
             */
            pending_rate_requests: number;
            /**
             * Format: int64
             * @description How many of those requests carry a final price.
             *
             *     Below `inference_requests` when some request has no rate to price it —
             *     the cost is then a floor, not a total. `getCostStateSummary` says why.
             */
            priced_requests: number;
            /**
             * Format: int64
             * @description Requests with complete rates but approximate provider usage.
             */
            provisional_requests: number;
            /**
             * Format: int64
             * @description Cost absorbed by provider-provisioned capacity, in micro-US-dollars.
             */
            provisioned_cost_microusd: number;
            /**
             * Format: int64
             * @description Cost a user's own subscription absorbed, in micro-US-dollars.
             */
            subscription_cost_microusd: number;
            /**
             * Format: int64
             * @description Requests served on a user subscription credential.
             */
            subscription_requests: number;
            /**
             * Format: int64
             * @description MCP tool calls.
             */
            tool_calls: number;
            /**
             * Format: int64
             * @description MCP request body bytes.
             */
            tool_request_bytes: number;
            /**
             * Format: int64
             * @description MCP response body bytes.
             */
            tool_response_bytes: number;
            /**
             * Format: int64
             * @description Requests with no usable provider token counts.
             */
            unpriceable_requests: number;
        };
        /**
         * @description Whether an account is a person or an automation's service identity.
         * @enum {string}
         */
        UserKind: "person" | "service";
        /**
         * @description Installation-level account role.
         * @enum {string}
         */
        UserRole: "administrator" | "user";
        /** @description One account's recorded traffic. */
        UserUsageView: {
            /**
             * Format: int64
             * @description Governed connected-app HTTP requests.
             */
            app_requests: number;
            /**
             * Format: int64
             * @description Estimated metered spend prompt caching avoided, in micro-US-dollars.
             *
             *     What these requests would have cost had every input token been billed
             *     at each event's snapshotted base input rate, minus what they were
             *     estimated at: the cache-read discount less the cache-write premium.
             *     Billed rows only, so it pairs with `estimated_cost_microusd`; floored,
             *     so a saving is never overstated; negative when write premiums exceeded
             *     read savings.
             */
            cache_savings_microusd: number;
            /**
             * Format: int64
             * @description Input tokens written to a provider prompt cache (cache writes).
             */
            cache_write_input_tokens: number;
            /**
             * Format: int64
             * @description Input tokens served from a provider prompt cache (cache reads).
             */
            cached_input_tokens: number;
            /**
             * Format: int64
             * @description Cost drawn from a user's prepaid usage credits (provider "extra
             *     usage"), in micro-US-dollars. Real prepaid balance was consumed, but
             *     not billed to the installation's metered account.
             */
            credits_cost_microusd: number;
            /** @description Display-ready account name. */
            display_name: string;
            /** @description Display-ready account email. */
            email: string;
            /**
             * Format: int64
             * @description Estimated metered cost, in micro-US-dollars.
             */
            estimated_cost_microusd: number;
            /**
             * Format: int64
             * @description Model requests recorded for this account.
             */
            inference_requests: number;
            /**
             * Format: int64
             * @description Input tokens across those requests.
             */
            input_tokens: number;
            /**
             * Format: int64
             * @description Output tokens across those requests.
             */
            output_tokens: number;
            /**
             * Format: int64
             * @description Requests awaiting one or more rate dimensions.
             */
            pending_rate_requests: number;
            /**
             * Format: int64
             * @description How many of those requests carry a final price.
             *
             *     Below `inference_requests` when some request has no rate to price it —
             *     the cost is then a floor, not a total. `getCostStateSummary` says why.
             */
            priced_requests: number;
            /**
             * Format: int64
             * @description Requests with complete rates but approximate provider usage.
             */
            provisional_requests: number;
            /**
             * Format: int64
             * @description Cost absorbed by provider-provisioned capacity, in micro-US-dollars.
             */
            provisioned_cost_microusd: number;
            /**
             * Format: int64
             * @description Cost a user's own subscription absorbed, in micro-US-dollars.
             *
             *     Notional: the installation was not billed for it. Reported separately so
             *     it is neither lost nor double-counted against metered spend.
             */
            subscription_cost_microusd: number;
            /**
             * Format: int64
             * @description Requests served on a user subscription credential.
             */
            subscription_requests: number;
            /**
             * Format: int64
             * @description MCP tool calls.
             */
            tool_calls: number;
            /**
             * Format: int64
             * @description Requests with no usable provider token counts.
             */
            unpriceable_requests: number;
            /**
             * Format: uuid
             * @description Stable user ID.
             */
            user_id: string;
        };
        /** @description One tier of a submitted waterfall. */
        WaterfallRouteInput: {
            /** @description Whether the tier participates in selection. Defaults to true. */
            enabled?: boolean;
            /**
             * Format: uuid
             * @description The provider model to place at this tier.
             */
            model_id: string;
        };
        /**
         * @description One end of a waterfall tier.
         *
         *     The gateway model that owns the waterfall is tier zero and is described by
         *     the same shape as its fallbacks, because what an administrator compares
         *     between tiers — which provider serves it, and which underlying model it
         *     claims to be, and what the gateway has lately observed of it — is the same
         *     in both places.
         */
        WaterfallRouteRef: {
            /**
             * @description Which underlying model this route serves. Every tier must agree with
             *     tier zero; null only for rows written by a pre-upgrade release, which
             *     are compared by `gateway_id` instead.
             */
            canonical_model_id?: string | null;
            /**
             * @description Identifier clients would request this route by directly.
             * @example claude-opus-5
             */
            gateway_id: string;
            health?: null | components["schemas"]["RouteHealthView"];
            /**
             * Format: uuid
             * @description The provider model serving this tier, not its client-visible name.
             */
            model_id: string;
            /** @description Owning provider name. */
            provider_name: string;
        };
        /** @description One configured fallback tier. */
        WaterfallRouteView: {
            /**
             * @description Whether the tier participates in selection right now, and what holds it
             *     out if not.
             */
            availability: components["schemas"]["RouteAvailabilityView"];
            /** @description Which underlying model this route serves. */
            canonical_model_id?: string | null;
            /**
             * @description Whether the administrator switched this tier on *in the waterfall*. This
             *     is the configured bit only: a tier can be on here and still never be
             *     tried, which is what `availability` reports. A disabled tier keeps its
             *     place in the order.
             */
            enabled: boolean;
            /** @description Identifier clients would request this route by directly. */
            gateway_id: string;
            health?: null | components["schemas"]["RouteHealthView"];
            /**
             * Format: uuid
             * @description The provider model serving this tier.
             */
            model_id: string;
            /**
             * Format: int32
             * @description Priority, counting from one. Tier zero is the primary and is reported as
             *     `primary` rather than in this list.
             */
            position: number;
            /** @description Owning provider name. */
            provider_name: string;
        };
        /** @description A set of enabled routes that already serve one underlying model. */
        WaterfallSuggestionGroupView: {
            /** @description The shared canonical identity these routes group on. */
            canonical_model_id: string;
            /**
             * @description Whether *any* member already anchors a configured waterfall, so a caller
             *     can offer review rather than proposing a list that exists. A summary of
             *     `members[].has_waterfall`; the members say which one.
             */
            has_waterfall: boolean;
            /** @description Candidate routes in proposed order; the first is the proposed tier zero. */
            members: components["schemas"]["WaterfallSuggestionMemberView"][];
        };
        /** @description One candidate route inside a suggested waterfall. */
        WaterfallSuggestionMemberView: {
            /**
             * @description The route's *current* canonical identity, so a caller sees what the
             *     group is built on rather than only what it would become.
             */
            canonical_model_id?: string | null;
            /** @description Identifier clients request this route by directly. */
            gateway_id: string;
            /**
             * @description Whether this member is itself the primary of a configured waterfall.
             *
             *     Per member rather than only per group, because which one anchors the
             *     existing waterfall is what a caller needs to offer review of it. Collapsed
             *     to a group-level boolean, a group with one configured member reads as
             *     "already handled" and a route added to it later is never proposed.
             */
            has_waterfall: boolean;
            /**
             * Format: uuid
             * @description The provider model that would serve this tier.
             */
            model_id: string;
            /** @description Owning provider name. */
            provider_name: string;
            /** @description Ordering class: `direct`, `hyperscaler`, or `aggregator`. */
            source_class: components["schemas"]["RouteSourceClassView"];
        };
        /** @description Suggested waterfalls, and the repairs that would make more of them possible. */
        WaterfallSuggestionsResponse: {
            /** @description Routes that appear to serve one model but carry differing canonical IDs. */
            canonical_mismatches: components["schemas"]["CanonicalMismatchView"][];
            /** @description Canonical groups holding more than one enabled route. */
            groups: components["schemas"]["WaterfallSuggestionGroupView"][];
        };
        /** @description One gateway model's failover waterfall. */
        WaterfallView: {
            /**
             * @description Whether the fallbacks are consulted at all. Under `managed` this is
             *     the installation's `automatic`; under `custom` it is this model's own
             *     switch, and false leaves the primary serving alone without losing the
             *     configured order.
             */
            enabled: boolean;
            /**
             * @description The installation default the model follows when `managed`, reported
             *     beside the waterfall so a caller can tell "managed and deriving" from
             *     "managed and off" without a second read.
             */
            installation: components["schemas"]["FailoverRoutingView"];
            /** @description Which regime governs this model. */
            policy: components["schemas"]["FailoverPolicyView"];
            /** @description The gateway model the waterfall belongs to; implicitly tier zero. */
            primary: components["schemas"]["WaterfallRouteRef"];
            /**
             * @description Fallback tiers in priority order. Under `managed` these are the routes
             *     the installation setting derives right now — reported so an
             *     administrator can see what automatic routing would try, and never
             *     stored.
             */
            routes: components["schemas"]["WaterfallRouteView"][];
            /**
             * @description Whether upstream throttling counts as an outage worth failing over on.
             *     Under `managed` this is the installation's trigger.
             */
            trigger_on_rate_limit: boolean;
        };
        /** @enum {string} */
        WindowKind: "daily" | "weekly" | "monthly";
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    listAllowedAddOnImages: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AllowedAddOnImageListResponse"];
                };
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    listAddOns: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnListResponse"];
                };
            };
        };
    };
    createAddOn: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddOnCreateRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnView"];
                };
            };
        };
    };
    getAddOn: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnView"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    updateAddOn: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddOnUpdateRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnView"];
                };
            };
        };
    };
    clearPreviousAddOnSecret: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    setAddOnEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["EnabledRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnView"];
                };
            };
        };
    };
    provisionAddOnForge: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Add-on slug */
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ProvisionAddOnForgeRequest"];
            };
        };
        responses: {
            /** @description Forge created and granted */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProvisionedAddOnForgeResponse"];
                };
            };
            /** @description No add-on has this slug */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Name or tool slug already in use, or the identity would hold two forge apps for github.com */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The add-on names no service identity, or none exists yet */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getAddOnHosting: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnHostingView"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    updateAddOnHosting: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddOnHostingUpdateRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnHostingView"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    updateAddOnHostingEnvironment: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddOnEnvironmentUpdateRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnHostingView"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    installAddOnHosting: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnHostingView"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    getAddOnHostingMetrics: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Add-on slug */
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The relayed summary */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnPairingReportResponse"];
                };
            };
            /** @description No such add-on or no hosting row */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The row is not an installed managed workload */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getAddOnHostingPairing: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Add-on slug */
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The relayed report */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnPairingReportResponse"];
                };
            };
            /** @description No such add-on or no hosting row */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The row is not an installed managed workload */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    mintAddOnWebhookSecret: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MintedWebhookSecretResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    upsertAddOnMachineBinding: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["MachineBindingRequest"];
            };
        };
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    listAddOnMachineBindings: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MachineBindingListResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    probeAddOn: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Add-on slug */
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Probe outcome */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProbeAddOnResponse"];
                };
            };
            /** @description No such add-on */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The add-on has no probeable machine URL */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    rotateAddOnSecret: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RotatedSecretResponse"];
                };
            };
        };
    };
    getAddOnSettings: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnSettingsResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    updateAddOnSettings: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": unknown;
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddOnSettingsResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    getTidebreakRuntime: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RuntimeView"];
                };
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    provisionTidebreakRuntime: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ProvisionRuntimeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProvisionRuntimeResponse"];
                };
            };
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    listAuditEvents: {
        parameters: {
            query?: {
                /** @description Keep only actions starting with this, e.g. `rate_limit.`. */
                action?: string;
                /** @description Keep only events taken by this account. */
                actor_id?: string;
                /** @description Keep only events taken by this kind of actor: `user` or `system`. Pass it with `actor_id`. */
                actor_type?: string;
                /** @description A `next_cursor` from an earlier response. */
                cursor?: string;
                /** @description Events per page, 1 to 200. Defaults to 50. */
                limit?: number;
                /** @description Keep only events with this terminal outcome. */
                outcome?: components["schemas"]["OperationOutcome"];
                /** @description Keep only events against this specific object. */
                target_id?: string;
                /** @description Keep only events against this kind of object. */
                target_type?: components["schemas"]["AuditTargetKind"];
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of the ledger */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AuditEventListResponse"];
                };
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description cursor, limit, outcome, target_type, target_id, actor_type, or actor_id is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getAuthenticationPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The policy */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AuthenticationPolicyView"];
                };
            };
        };
    };
    listMyConnectedAppConnections: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The caller's connected-app connection status */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppConnectionListResponse"];
                };
            };
        };
    };
    disconnectMyConnectedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The caller is disconnected */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    listConnectedApps: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Connected apps */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppListResponse"];
                };
            };
        };
    };
    createConnectedApp: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectedAppRequest"];
            };
        };
        responses: {
            /** @description App created, disabled */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppView"];
                };
            };
            /** @description The name or tool slug is taken, or the app is the built-in one */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The settings were rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteConnectedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description App deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is referenced, or is the built-in app */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listConnectedAppAccessPolicies: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Statements */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppAccessPolicyListResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createConnectedAppAccessPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectedAppAccessPolicyRequest"];
            };
        };
        responses: {
            /** @description Statement created */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppAccessPolicyResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Statement refused */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteConnectedAppAccessPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
                /** @description Statement ID */
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Statement deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app or statement */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppAgentAccess: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppAgentAccessRequest"];
            };
        };
        responses: {
            /** @description Interfaces updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app has no buffered REST channel */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppAudience: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppAudienceRequest"];
            };
        };
        responses: {
            /** @description Audience updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not one the gateway provisions */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getConnectedAppCaTrust: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description What this app trusts */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppCaTrustView"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppCaTrust: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppCaTrustRequest"];
            };
        };
        responses: {
            /** @description Trust bundle updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The bundle was refused */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteConnectedAppCaTrust: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Trust bundle cleared */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    computeConnectedAppEffectiveAccess: {
        parameters: {
            query?: {
                /** @description Judge as a sandbox spawned under this profile. */
                sandbox_profile_id?: string | null;
                /** @description Judge as this user (adds their user- and team-scope statements). */
                user_id?: string | null;
            };
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Judged ceiling */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectedAppEffectiveAccessResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Ceiling not enumerable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getGitForgeGithubApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The forge's GitHub App */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GitForgeGithubAppView"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge could not be reached */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setGitForgeGithubAppInstallation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetGitForgeGithubAppInstallationRequest"];
            };
        };
        responses: {
            /** @description Installation recorded */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The installation identifier is not usable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGitForgeGithubAppInstallations: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Approved installations */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GitForgeGithubAppInstallationListResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge holds no signing key yet */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge could not be reached */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGitForgeInstallationRepositories: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Repositories the installation covers */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GitForgeInstallationRepositoryListResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge is not ready to mint */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge could not be reached */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGitForgeWipRefs: {
        parameters: {
            query: {
                /** @description owner/repo as the forge names it */
                repository: string;
            };
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description WIP refs on the repository */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GitForgeWipRefListResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The repository is not usable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge could not be reached */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteGitForgeWipRef: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DeleteGitForgeWipRefRequest"];
            };
        };
        responses: {
            /** @description Ref deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app or ref */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is not a github_app git forge */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The repository or ref name is not usable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The forge could not be reached */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppModelPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppModelPolicyRequest"];
            };
        };
        responses: {
            /** @description Policy replaced */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EndpointCascade"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A Direct endpoint stands in the way */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The route list was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppName: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppNameRequest"];
            };
        };
        responses: {
            /** @description App renamed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such live app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name is taken, or this is the built-in app */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name is empty or too long */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppOauthScopes: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConnectedAppOauthScopesRequest"];
            };
        };
        responses: {
            /** @description OAuth scopes updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such live app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app has no editable OAuth scopes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Scopes exceed the stored bound or contain control characters */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    purgeConnectedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description App permanently removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app is live, or something still references it */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConnectedAppPersonalCredentialPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPersonalCredentialPolicyRequest"];
            };
        };
        responses: {
            /** @description Policy updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    probeConnectedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Probe outcome */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProbeConnectedAppResponse"];
                };
            };
            /** @description No such app */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app's endpoint URL is not probeable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listDeletedConnectedApps: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Deleted connected apps */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeletedConnectedAppListResponse"];
                };
            };
        };
    };
    createConnectorAccessProfile: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectorAccessProfileRequest"];
            };
        };
        responses: {
            /** @description Created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectorAccessProfileView"];
                };
            };
        };
    };
    createConnectorConnection: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectorConnectionRequest"];
            };
        };
        responses: {
            /** @description Created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectorConnectionView"];
                };
            };
        };
    };
    createConnectorGrant: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectorGrantRequest"];
            };
        };
        responses: {
            /** @description Created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectorGrantView"];
                };
            };
        };
    };
    listConnectorSites: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Sites */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectorSiteListResponse"];
                };
            };
        };
    };
    createConnectorSite: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectorSiteRequest"];
            };
        };
        responses: {
            /** @description Created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectorSiteView"];
                };
            };
        };
    };
    publishConnectorSnapshot: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                site_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishConnectorSnapshotRequest"];
            };
        };
        responses: {
            /** @description Published */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PublishedConnectorSnapshot"];
                };
            };
        };
    };
    createConnector: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConnectorInstanceRequest"];
            };
        };
        responses: {
            /** @description Created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedConnectorInstance"];
                };
            };
        };
    };
    disableConnector: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                connector_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Disabled */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    listConversations: {
        parameters: {
            query?: {
                /** @description A `next_cursor` from an earlier response. */
                cursor?: string;
                /** @description `asc` or `desc`; defaults to `desc`. */
                direction?: string;
                /** @description Keep only this harness family, e.g. `codex`. */
                harness?: string;
                /** @description Conversations per page, 1 to 200. Defaults to 50. */
                limit?: number;
                /** @description Substring match on title, repo, workspace, owner, or id. */
                q?: string;
                /** @description Exact match on the latest observed `owner/repo` handle. */
                repo_slug?: string;
                /** @description Inclusive RFC3339 lower bound on counted activity. */
                since?: string;
                /** @description Ordering key: `last_activity` (default) or `first_seen` page by cursor; `spend`, `subscription`, `provisioned`, `tokens`, and `requests` rank by sums over the selected window and return a capped list with no cursor. */
                sort?: string;
                /** @description Exclusive RFC3339 upper bound on counted activity. */
                until?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of conversations */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationListResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description cursor, limit, since, until, harness, repo_slug, q, sort, or direction is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getConversation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Conversation ID */
                conversation_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Conversation detail */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No conversation has this ID, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getCostControlSettings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current cost-control settings */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostControlSettingsView"];
                };
            };
        };
    };
    updateCostControlSettings: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateCostControlSettingsRequest"];
            };
        };
        responses: {
            /** @description Updated cost-control settings */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostControlSettingsView"];
                };
            };
            /** @description time_zone is not a known IANA name */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listCostLimits: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Cost limits and their current state */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostLimitListResponse"];
                };
            };
        };
    };
    createCostLimit: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateCostLimitRequest"];
            };
        };
        responses: {
            /** @description Cost limit created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostLimitResponse"];
                };
            };
            /** @description A policy already covers this scope, traffic, and window */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A field is unusable: scope_id, limit_microusd, or a reserved scope */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateCostLimit: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Policy id from `listCostLimits`. */
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateCostLimitRequest"];
            };
        };
        responses: {
            /** @description Updated cost limit and its current state */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostLimitResponse"];
                };
            };
            /** @description No updatable policy with this id */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description limit_microusd is negative */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteCostLimit: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Policy id from `listCostLimits`. */
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Cost limit deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No deletable policy with this id */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getMyCostLimitStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Cost limits applicable to the caller */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostLimitListResponse"];
                };
            };
        };
    };
    listCostFindings: {
        parameters: {
            query?: {
                /** @description Inclusive RFC3339 lower bound on occurred_at. */
                since?: string;
                /** @description Exclusive RFC3339 upper bound on occurred_at. */
                until?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Advisory cost findings */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostFindingsResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description since or until is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getCostStateSummary: {
        parameters: {
            query?: {
                /** @description Restrict to one catalog model. */
                model_id?: string;
                /** @description Inclusive RFC3339 lower bound on occurred_at. */
                since?: string;
                /** @description Exclusive RFC3339 upper bound on occurred_at. */
                until?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Cost-state counts */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostStateSummaryResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description model_id, since, or until is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getGuardrailActivity: {
        parameters: {
            query?: {
                /** @description Days of history (1-90, default 7). Out of range is refused, not clamped. */
                days?: number;
                /** @description One or two comma-separated dimensions: team, model. team_id and model_id are accepted aliases. */
                group_by?: string;
                /** @description One of inference_request, tool_call, tool_result, app_egress. */
                surface?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailActivityResponse"];
                };
            };
            /** @description days, surface, or group_by is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailEngines: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailEngineListResponse"];
                };
            };
        };
    };
    createGuardrailEngine: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGuardrailEngineRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailEngineResponse"];
                };
            };
            /** @description Another engine already uses the key */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The key, kind, or config was refused */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateGuardrailEngine: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                engine_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateGuardrailEngineRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailEngineResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Another engine already uses the key */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The key, kind, or config was refused */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteGuardrailEngine: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                engine_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A policy still chains the engine, or the engine is seeded */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    previewGuardrailEngine: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PreviewGuardrailEngineRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PreviewGuardrailEngineResponse"];
                };
            };
            /** @description No such engine instance */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Preview rate limit reached */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The request named neither an instance nor a config, the config does not compile, the kind is not in-process, or the sample is too large */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    wipeGuardrailEvents: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailEventWipeResponse"];
                };
            };
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Active writers or retained history exceeded the bounded wipe window */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailFindingPatternIgnores: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailFindingPatternIgnoreListResponse"];
                };
            };
        };
    };
    ignoreGuardrailFindingPattern: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["IgnoreGuardrailFindingPatternRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailFindingPatternIgnoreResponse"];
                };
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    unignoreGuardrailFindingPattern: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                ignore_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Pattern ignore removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailFindingGroups: {
        parameters: {
            query?: {
                /** @description A next_cursor from an earlier response. */
                cursor?: string;
                /** @description Only groups whose latest occurrence falls in the last N days (1-90). Counts stay lifetime totals. Defaults to all retained history. */
                days?: number;
                /** @description Unique findings per page, 1 to 100. Defaults to 25. */
                limit?: number;
                /** @description open (default), dismissed, ignored, reviewed (dismissed plus rule-ignored), or all. */
                status?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailFindingGroupListResponse"];
                };
            };
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    reviewGuardrailFinding: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                finding_key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReviewGuardrailFindingRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailFindingReviewResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailPolicies: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailPolicyListResponse"];
                };
            };
        };
    };
    createGuardrailPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGuardrailPolicyRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailPolicyResponse"];
                };
            };
        };
    };
    updateGuardrailPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateGuardrailPolicyRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailPolicyResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The request is invalid, or the longer engine list would exceed a scope's engine-chain cap */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteGuardrailPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createGuardrailAttachment: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGuardrailAttachmentRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailAttachmentResponse"];
                };
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The scope is unavailable, or the attachment would exceed the scope's engine-chain cap */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteGuardrailAttachment: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                attachment_id: string;
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailTemplates: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailTemplateListResponse"];
                };
            };
        };
    };
    applyGuardrailTemplate: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                template_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ApplyGuardrailTemplateRequest"];
            };
        };
        responses: {
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AppliedGuardrailTemplateResponse"];
                };
            };
            /** @description This gateway's catalog carries no such template */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The names this apply would create are taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A required parameter is missing, a parameter value is unusable, or the scope's engine-chain cap would be exceeded */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listGuardrailVerdicts: {
        parameters: {
            query?: {
                /** @description A `next_cursor` from an earlier response. */
                cursor?: string;
                /** @description Relative window in days (1-90). Narrows alongside since/until rather than replacing them. Omitted means all retained history. */
                days?: number;
                /** @description `flag` for matches only, or `pass`. */
                decision?: string;
                /** @description Only verdicts from this engine, e.g. builtin.secrets. */
                engine_key?: string;
                /** @description Verdicts per page, 1 to 200. Defaults to 50. */
                limit?: number;
                /** @description Only verdicts recorded by this policy. */
                policy_id?: string;
                /** @description Inclusive RFC3339 lower bound on occurred_at. */
                since?: string;
                /** @description One of inference_request, tool_call, tool_result, app_egress. */
                surface?: string;
                /** @description Exclusive RFC3339 upper bound on occurred_at. */
                until?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description One page of verdicts */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardrailVerdictListResponse"];
                };
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A filter, the cursor, or the limit is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listIdentityProviders: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Identity providers */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IdentityProviderListResponse"];
                };
            };
        };
    };
    createIdentityProvider: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateIdentityProviderRequest"];
            };
        };
        responses: {
            /** @description Provider created, disabled and unverified */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IdentityProviderView"];
                };
            };
            /** @description The name or issuer is already configured */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A field was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getIdentityProvider: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Identity provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The provider */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IdentityProviderView"];
                };
            };
            /** @description No such provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteIdentityProvider: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Identity provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Provider deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider is configuration-owned, still linked, or the last way in */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setIdentityProviderEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Identity provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetIdentityProviderEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider is configuration-owned, unverified, or the last way in */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setIdentityProviderProvisioning: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Identity provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetIdentityProviderProvisioningRequest"];
            };
        };
        responses: {
            /** @description Provisioning terms updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider is configuration-owned */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A domain was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getInferenceRequest: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description The `x-request-id` echoed on the inference response. Recorded IDs are UUIDs; a non-UUID value is refused as `invalid_request_id`. */
                request_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Matching events, possibly none */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["InferenceRequestView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The traced value is not a UUID, so it was never recorded */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listIssues: {
        parameters: {
            query: {
                /** @description Include groups an administrator dismissed. */
                include_dismissed: boolean;
                /** @description Inclusive RFC3339 lower bound. */
                since?: string;
                /** @description Exclusive RFC3339 upper bound. */
                until?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Grouped operator issues */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuesResponse"];
                };
            };
            /** @description Missing scope or administrator role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description since or until is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    dismissIssue: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Stable issue key from listIssues */
                issue_key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Issue dismissed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Issue key is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    restoreIssue: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Stable issue key from listIssues */
                issue_key: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Issue restored or already active */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Issue key is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listMcpEndpoints: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description MCP endpoints */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["McpEndpointListResponse"];
                };
            };
        };
    };
    createMcpEndpoint: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateMcpEndpointRequest"];
            };
        };
        responses: {
            /** @description Endpoint created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedMcpEndpoint"];
                };
            };
            /** @description The slug is taken, or the tier cannot carry an app */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name, slug, or app list was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getMcpEndpoint: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The endpoint */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["McpEndpointView"];
                };
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteMcpEndpoint: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Endpoint deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The endpoint belongs to a user */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setMcpEndpointApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMcpEndpointAppRequest"];
            };
        };
        responses: {
            /** @description Assignment updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The tier cannot carry this app, or the endpoint belongs to a user */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setMcpEndpointEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMcpEndpointEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The endpoint belongs to a user */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setMcpEndpointExecutionMode: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMcpExecutionModeRequest"];
            };
        };
        responses: {
            /** @description Tier updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description An app the endpoint carries requires attestation, or the endpoint belongs to a user */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setMcpEndpointName: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMcpEndpointNameRequest"];
            };
        };
        responses: {
            /** @description Name updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such endpoint */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The endpoint belongs to a user */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name is empty or too long */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setMcpEndpointSandboxProfile: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description MCP endpoint ID */
                endpoint_id: string;
                /** @description Sandbox profile ID */
                profile_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMcpEndpointSandboxProfileRequest"];
            };
        };
        responses: {
            /** @description Attachment updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such endpoint or profile */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderSubscriptionFallback: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProviderSubscriptionFallbackRequest"];
            };
        };
        responses: {
            /** @description Fallback preference updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No active caller-owned binding matches the ID */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderSubscriptionLabel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProviderSubscriptionLabelRequest"];
            };
        };
        responses: {
            /** @description Label updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No active caller-owned binding matches the ID */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Another active account already uses this label */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The label is empty or longer than 120 characters */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    resetProviderSubscriptionCredits: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ResetProviderSubscriptionCreditsRequest"];
            };
        };
        responses: {
            /** @description The provider processed the reset request */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ResetProviderSubscriptionCreditsResponse"];
                };
            };
            /** @description No active caller-owned binding matches the ID */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The binding must be reconnected or reauthorized */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Identifiers are invalid or this provider has no reset-credit surface */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider account service is unavailable */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    shareProviderSubscriptionWithTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
                /** @description Team receiving the binding grant */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ShareProviderSubscriptionRequest"];
            };
        };
        responses: {
            /** @description The team grant exists */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No active caller-owned binding matches the ID */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Sharing is disabled, the target team does not exist, or the share policy is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    unshareProviderSubscriptionFromTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
                /** @description Team whose grant is removed */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The team grant was removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The caller does not own this binding/share pair */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    refreshProviderSubscriptionUsage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Caller-owned subscription binding ID */
                binding_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Credential health and supported usage were refreshed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No active caller-owned binding matches the ID */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The binding must be reconnected or reauthorized */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider refresh or usage service is unavailable */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listModelProviders: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Configured providers */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderListResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createModelProvider: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateProviderRequest"];
            };
        };
        responses: {
            /** @description Provider created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderView"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A rule rejected the request */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteModelProvider: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Provider deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelProviderApiKey: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProviderApiKeyRequest"];
            };
        };
        responses: {
            /** @description Key stored */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider kind does not hold an installation API key */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No key was supplied */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setBedrockProviderPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetBedrockPolicyRequest"];
            };
        };
        responses: {
            /** @description Policy updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider does not speak Bedrock */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The project was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description AWS refused the call or was unreachable */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    runModelProviderDiscovery: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The upstream answered */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderDiscoveryResponse"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider has no credential that can enumerate models */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The upstream refused the call or was unreachable */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelProviderEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelProviderName: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProviderNameRequest"];
            };
        };
        responses: {
            /** @description Name updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Another provider already answers to that name */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name is empty or too long */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelProviderOpenAiApi: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProviderWireApiRequest"];
            };
        };
        responses: {
            /** @description Southbound API updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider kind has no configurable OpenAI API */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getModelProviderPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current policy */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderPolicyView"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelProviderSubscriptionPolicy: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetSubscriptionPolicyRequest"];
            };
        };
        responses: {
            /** @description Policy updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider kind cannot carry subscriptions */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listProviderSubscriptions: {
        parameters: {
            query?: {
                /** @description Inclusive RFC3339 lower bound for `borrowed_request_count`. */
                since?: string;
                /** @description Exclusive RFC3339 upper bound for `borrowed_request_count`. */
                until?: string;
            };
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Attached subscriptions */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProviderSubscriptionListResponse"];
                };
            };
            /** @description Caller is not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description since or until is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    resetProviderSubscriptionLimitState: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Binding ID from `listProviderSubscriptions` */
                binding_id: string;
                /** @description Provider the binding belongs to */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The binding is available for the next request */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such active binding under this provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    revokeProviderSubscription: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Binding ID from `listProviderSubscriptions` */
                binding_id: string;
                /** @description Provider the binding belongs to */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Binding revoked and its credentials destroyed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such binding under this provider */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Binding is already revoked */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    enforceBedrockZeroDataRetention: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Provider ID */
                provider_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description AWS accepted the change */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Provider is not a Bedrock provider whose policy is `none` */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description AWS refused the call or was unreachable */
            502: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getConversationTitleModelTask: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Current task route */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationTitleTaskView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setConversationTitleModelTask: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetConversationTitleTaskRequest"];
            };
        };
        responses: {
            /** @description Updated task route */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationTitleTaskView"];
                };
            };
            /** @description Missing write scope or administrator role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The selected model is absent, disabled, or serves no supported protocol */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listProviderModels: {
        parameters: {
            query?: {
                /** @description Restrict to enabled (`true`) or disabled (`false`) models. */
                enabled?: boolean;
                /** @description Restrict by rate completeness. */
                pricing?: components["schemas"]["PricingCompleteness"];
                /** @description Restrict to models under one provider. */
                provider_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Catalog models */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelListResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A filter value is not recognized */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createProviderModel: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateModelRequest"];
            };
        };
        responses: {
            /** @description Model created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelView"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description gateway_id or provider upstream ID already exists */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A rule rejected the request */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteProviderModel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Model deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelAnthropicMessageBatches: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetAnthropicMessageBatchesRequest"];
            };
        };
        responses: {
            /** @description Capability updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The route cannot serve Anthropic Message Batches */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setModelCanonicalId: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetCanonicalModelIdRequest"];
            };
        };
        responses: {
            /** @description Canonical model ID updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A configured waterfall names this model and the change would break it */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The identifier is not a legal canonical model ID */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listProviderModelCostMultipliers: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Multipliers in force */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostMultipliersResponse"];
                };
            };
        };
    };
    setProviderModelCostMultiplier: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetCostMultiplierRequest"];
            };
        };
        responses: {
            /** @description Multiplier applied, or already in force */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CostMultiplierUpdateView"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The multiplier or its geography was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelDisplayName: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetDisplayNameRequest"];
            };
        };
        responses: {
            /** @description Display name updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Display name is blank or too long */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listProviderModelGrants: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Who holds the model */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ModelAccessResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelLimits: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetLimitsRequest"];
            };
        };
        responses: {
            /** @description Limits updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Limits are not positive or are inverted */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listProviderModelPrices: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Rate timeline */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PriceHistoryResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    cancelScheduledProviderModelPrice: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Interval ID from listProviderModelPrices */
                interval_id: string;
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The pending change was withdrawn */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such model, or no such interval on it */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The interval has already taken effect */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelPricing: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPricingRequest"];
            };
        };
        responses: {
            /** @description Rates applied or scheduled */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PricingUpdateView"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A change is already pending, or the requested schedule would change nothing */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A rate or the effective instant was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelProvisionedCapacity: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetProvisionedCapacityRequest"];
            };
        };
        responses: {
            /** @description Provisioned-capacity mode updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing write scope or administrator role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The provider or route cannot use the selected mode */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setProviderModelReasoningEfforts: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetReasoningEffortsRequest"];
            };
        };
        responses: {
            /** @description Reasoning efforts updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getModelWaterfall: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID of the gateway model */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The model's waterfall */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WaterfallView"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    replaceModelWaterfall: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID of the gateway model */
                model_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ReplaceWaterfallRequest"];
            };
        };
        responses: {
            /** @description Waterfall replaced */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Model does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A rule rejected the submitted routes */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getFailoverRouting: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The installation default */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FailoverRoutingView"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateFailoverRouting: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateFailoverRoutingRequest"];
            };
        };
        responses: {
            /** @description The updated default */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FailoverRoutingView"];
                };
            };
            /** @description Missing write scope or administrator role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    importProviderModels: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ImportModelsRequest"];
            };
        };
        responses: {
            /** @description Per-item outcomes; inspect `data[].error` */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ImportModelsResponse"];
                };
            };
            /** @description Provider does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The batch itself is invalid (empty, too large, or unknown grantees) */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listRouteAffinity: {
        parameters: {
            query?: {
                /** @description Only pins held by this conversation. Excludes every derived pin, which belongs to no conversation. */
                conversation_id?: string;
                /** @description Page bound, default 100. Values outside 1..=500 are refused rather than clamped. */
                limit?: number;
                /** @description Only pins scoped to this gateway model's waterfall. */
                model_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Live pins */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RouteAffinityListResponse"];
                };
            };
            /** @description Out-of-range limit */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listWaterfallSuggestions: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Suggested waterfalls */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["WaterfallSuggestionsResponse"];
                };
            };
            /** @description Missing scope or role */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listPeople: {
        parameters: {
            query?: {
                /** @description `asc` or `desc`. Defaults to `asc`: a directory reads A to Z. */
                direction?: string;
                /** @description Rows to return, 1 to 500. Defaults to 500. */
                limit?: number;
                /** @description Case-insensitive name, email, or username search. */
                search?: string;
                /** @description Ordering key: `name` (default) or `email`. */
                sort?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Directory users */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PersonListResponse"];
                };
            };
            /** @description Caller is not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createPerson: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreatePersonRequest"];
            };
        };
        responses: {
            /** @description Account created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PersonView"];
                };
            };
            /** @description Email or username already in use */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A field or the sign-in mode was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updatePerson: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description User ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdatePersonRequest"];
            };
        };
        responses: {
            /** @description Account updated */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PersonView"];
                };
            };
            /** @description No such account */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Refused to protect the installation, or a duplicate */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A field was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setPersonModelGrant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Catalog model ID */
                model_id: string;
                /** @description User ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPersonModelGrantRequest"];
            };
        };
        responses: {
            /** @description Grant updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such account */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Unknown model */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setPersonPassword: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description User ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPersonPasswordRequest"];
            };
        };
        responses: {
            /** @description Password set */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such account */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The installation owner's credentials are protected */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The password was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    removePersonPassword: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description User ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Password removed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such account */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The account would be left unable to sign in, or is the installation owner */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listRateLimits: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Rate limits and their live readings */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RateLimitListResponse"];
                };
            };
        };
    };
    createRateLimit: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRateLimitRequest"];
            };
        };
        responses: {
            /** @description Rate limit created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RateLimitResponse"];
                };
            };
            /** @description A policy already covers this scope and dimension */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A field is unusable: scope_id, limit_value, or a reserved scope */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateRateLimit: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Policy id from `listRateLimits`. */
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRateLimitRequest"];
            };
        };
        responses: {
            /** @description Updated rate limit and its live reading */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RateLimitResponse"];
                };
            };
            /** @description No policy with this id */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description limit_value is negative */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteRateLimit: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Policy id from `listRateLimits`. */
                policy_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Rate limit deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No policy with this id */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getMyRateLimitStatus: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RateLimitListResponse"];
                };
            };
        };
    };
    listRateLimitTemplates: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Suggested templates and their rationale */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RateLimitTemplateListResponse"];
                };
            };
        };
    };
    getSandboxConcurrency: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The allowance and its current usage */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxConcurrencyResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxEgressBundles: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Bundles in name order */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EgressBundleListResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createSandboxEgressBundle: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSandboxEgressBundleRequest"];
            };
        };
        responses: {
            /** @description Bundle created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedSandboxEgressBundle"];
                };
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or the name is taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateSandboxEgressBundle: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Egress bundle ID */
                bundle_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSandboxEgressBundleRequest"];
            };
        };
        responses: {
            /** @description Domain set replaced */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such bundle */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteSandboxEgressBundle: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Egress bundle ID */
                bundle_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Bundle deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such bundle */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or a live profile still references the bundle */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxProfiles: {
        parameters: {
            query?: {
                /** @description Restrict to the profiles attached to this MCP endpoint. */
                endpoint_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Matching profiles */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxProfileListResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createSandboxProfile: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSandboxProfileRequest"];
            };
        };
        responses: {
            /** @description Profile created, closed */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedSandboxProfile"];
                };
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or the name is taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateSandboxProfile: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox profile ID */
                profile_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateSandboxProfileRequest"];
            };
        };
        responses: {
            /** @description Specification replaced */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such profile */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteSandboxProfile: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox profile ID */
                profile_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Profile retired */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such profile */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setSandboxProfileEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox profile ID */
                profile_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetSandboxProfileEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such profile */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxRunnerClasses: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Classes smallest rank first */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RunnerClassListResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createSandboxRunnerClass: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSandboxRunnerClassRequest"];
            };
        };
        responses: {
            /** @description Class created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedSandboxRunnerClass"];
                };
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or the name or rank is taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    updateSandboxRunnerClass: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Runner class ID */
                runner_class_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateSandboxRunnerClassRequest"];
            };
        };
        responses: {
            /** @description Specification replaced */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such class */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or the rank is taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The specification was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteSandboxRunnerClass: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Runner class ID */
                runner_class_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Class deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such class */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or a live profile still permits the class */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxes: {
        parameters: {
            query?: {
                /** @description Sandboxes to return, 1 to 200. Defaults to 200. */
                limit?: number;
                /** @description Restrict to sandboxes spawned from this conversation. */
                spawning_conversation_id?: string;
                /** @description Restrict to one lifecycle state. Singular alias for `states`; passing both is refused. */
                state?: components["schemas"]["SandboxState"];
                /** @description Restrict to a comma-separated set of lifecycle states, each named at most once. */
                states?: string;
                /** @description Restrict to one owning user. Administrators only. */
                user_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Matching sandboxes */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxListResponse"];
                };
            };
            /** @description Missing scope, or a member asked for another user's sandboxes */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description limit is out of range, or the state filter is unusable */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getSandbox: {
        parameters: {
            query?: {
                /** @description Return events after this sequence. Omit for the recent tail. */
                after_seq?: number;
                /** @description Events to return, 1 to 500. Defaults to 500. */
                limit?: number;
            };
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The sandbox and its recent events */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxDetailView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description after_seq or limit is out of range */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxAppHttp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description App HTTP attributed to the sandbox */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxAppHttpListView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    cancelSandbox: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        /** @description Optional: why the sandbox is being stopped */
        requestBody: {
            content: {
                "application/json": components["schemas"]["CancelSandboxRequest"];
            };
        };
        responses: {
            /** @description Cancellation recorded */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Missing scope, or not an administrator */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, or the sandbox already finished */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxEgress: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Egress records attributed to the sandbox */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxEgressListView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSandboxInference: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Inference attributed to the sandbox */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxInferenceListView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getSandboxContainerLogs: {
        parameters: {
            query?: {
                /** @description Exact container name: workload, sidecar, or packet-rules */
                container?: string;
                /** @description Continue one truncated container from its last delivered position */
                cursor?: string;
                /** @description Stream as SSE when true */
                follow?: boolean;
                /** @description Most recent lines per selected container */
                tail?: number;
            };
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Framed container output (JSON snapshot or SSE stream) */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxContainerLogsView"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Unknown container, invalid cursor, or follow combined with cursor */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    sendSandboxMessage: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Sandbox ID */
                sandbox_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SendSandboxMessageRequest"];
            };
        };
        responses: {
            /** @description The message is recorded and ordered */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SandboxMessageReceiptView"];
                };
            };
            /** @description The body is empty or larger than 32 KiB */
            400: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such sandbox, or it belongs to another user */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description This installation does not run sandboxes, the sandbox already finished, or its inbox is full */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listScimConnectors: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description SCIM connectors */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ScimConnectorListResponse"];
                };
            };
        };
    };
    createScimConnector: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateScimConnectorRequest"];
            };
        };
        responses: {
            /** @description Connector created, with its token */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["CreatedScimConnectorView"];
                };
            };
            /** @description The name is already in use */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setScimConnectorEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description SCIM connector ID */
                connector_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetScimConnectorEnabledRequest"];
            };
        };
        responses: {
            /** @description Connector retired */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No such connector */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Reactivation is not offered */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listServiceIdentities: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Service identities */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ServiceIdentityListResponse"];
                };
            };
        };
    };
    createServiceIdentity: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateServiceIdentityRequest"];
            };
        };
        responses: {
            /** @description Service identity created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ServiceIdentityView"];
                };
            };
            /** @description Username already taken */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Invalid username or unknown team */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setServiceIdentityAppGrant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Connected app ID */
                app_id: string;
                /** @description Service identity ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetServiceIdentityAppGrantRequest"];
            };
        };
        responses: {
            /** @description Grant updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The grant would leave the identity two forge apps for one origin */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No service identity has this ID, unknown app, or the built-in app */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setServiceIdentityEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Service identity ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetServiceIdentityEnabledRequest"];
            };
        };
        responses: {
            /** @description State updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No service identity has this ID */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setServiceIdentityTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Service identity ID */
                user_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetServiceIdentityTeamRequest"];
            };
        };
        responses: {
            /** @description Owning team updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description No service identity has this ID, or no team has this ID */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listSharedApps: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Reachable shared apps */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SharedAppListResponse"];
                };
            };
        };
    };
    createSharedApp: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSharedAppRequest"];
            };
        };
        responses: {
            /** @description Draft created with revision 1 */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SharedAppCreatedView"];
                };
            };
            /** @description This author already has an app with that slug */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The name, slug, manifest, or bundle was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    getSharedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The shared app */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SharedAppDetailView"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteSharedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description App deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The caller is not the author */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    disableSharedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description App stopped */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The caller neither authored the app, manages a team it was published to, nor administers the gateway */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    publishSharedApp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishSharedAppRequest"];
            };
        };
        responses: {
            /** @description Published and granted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The caller is not the author */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app was stopped */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The team or the current manifest was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    createSharedAppRevision: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateSharedAppRevisionRequest"];
            };
        };
        responses: {
            /** @description Revision appended */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SharedAppRevisionView"];
                };
            };
            /** @description The caller is not the author */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The app was stopped */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description The manifest or bundle was rejected */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    revokeSharedAppTeamGrant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Shared app ID */
                shared_app_id: string;
                /** @description Team whose grant is withdrawn */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description The team no longer reaches the app */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description The caller is neither the author nor a manager of this team */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description No such app, or it is not reachable */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listTeams: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Teams */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TeamListResponse"];
                };
            };
        };
    };
    createTeam: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateTeamRequest"];
            };
        };
        responses: {
            /** @description Team created */
            201: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TeamView"];
                };
            };
            /** @description Name or slug already used */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Name or slug is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    deleteTeam: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Team deleted */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setTeamAppGrant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetAppGrantRequest"];
            };
        };
        responses: {
            /** @description Grant updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Unknown app, or the built-in app */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setTeamEnabled: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetTeamEnabledRequest"];
            };
        };
        responses: {
            /** @description Team state updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listTeamMembers: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Team memberships */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["TeamMembershipListResponse"];
                };
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setTeamMembership: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetMembershipRequest"];
            };
        };
        responses: {
            /** @description Membership updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Unknown membership action or unknown user */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setTeamModelGrant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetModelGrantRequest"];
            };
        };
        responses: {
            /** @description Grant updated */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Unknown model */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    setTeamName: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Team ID */
                team_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetTeamNameRequest"];
            };
        };
        responses: {
            /** @description Team renamed */
            204: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
            /** @description Team does not exist */
            404: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Another team already uses the name */
            409: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description Name is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
    listUsage: {
        parameters: {
            query?: {
                /** @description Restrict inference usage to one authenticating client. */
                client_name?: components["schemas"]["GatewayClient"];
                /** @description One to three comma-separated dimensions: user, team, model, client, day, sandbox, repo, execution_kind. */
                group_by?: string;
                /** @description Restrict inference usage to one catalog model. */
                model_id?: string;
                /** @description Restrict inference usage to one host-stripped `owner/repo` recorded on the events. */
                repo_slug?: string;
                /** @description Restrict inference usage to one sandbox. */
                sandbox_id?: string;
                /** @description Inclusive RFC3339 lower bound on occurred_at. */
                since?: string;
                /** @description Restrict inference usage to one attributed team. */
                team_id?: string;
                /** @description Exclusive RFC3339 upper bound on occurred_at. */
                until?: string;
                /** @description Restrict inference usage to one account. */
                user_id?: string;
            };
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Usage rollups */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["UsageResponse"];
                };
            };
            /** @description Missing scope */
            403: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
            /** @description A filter or grouping value is invalid */
            422: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ApiErrorResponse"];
                };
            };
        };
    };
}
