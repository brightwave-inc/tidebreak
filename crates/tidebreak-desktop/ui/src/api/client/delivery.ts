import type {
  CodeDeliveryActionResult,
  CodeDeliveryPullRequestActionBody,
  CodeDeliveryPullRequestDetail,
  CodeDeliveryPullRequestQuery,
  CodeDeliveryPullRequestsPage,
  CodeDeliveryPullRequestTarget,
  CodeDeliveryRepositoriesSnapshot,
  CodeDeliveryRunActionBody,
  CodeDeliveryRunDetail,
  CodeDeliveryRunQuery,
  CodeDeliveryRunsPage,
  CodeDeliveryRunTarget,
} from "../types";
import {
  type Constructor,
  type DeliveryRequestOptions,
  HttpCore,
  requireParsed,
} from "./http";
import {
  parseCodeDeliveryActionResult,
  parseCodeDeliveryPullRequestDetail,
  parseCodeDeliveryPullRequestsPage,
  parseCodeDeliveryRepositories,
  parseCodeDeliveryRunDetail,
  parseCodeDeliveryRunsPage,
} from "../../code/parsers";

/** Install-wide GitHub delivery reads and actions. */
export function withDeliveryApi<TBase extends Constructor<HttpCore>>(
  Base: TBase,
) {
  return class extends Base {
    async getCodeDeliveryRepositories(
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryRepositoriesSnapshot> {
      const path = options?.refreshAuth
        ? "/code/delivery/repositories?refresh=true"
        : "/code/delivery/repositories";
      return requireParsed(
        parseCodeDeliveryRepositories(
          await this.deliveryJson(
            path,
            {
              headers: this.headers(),
            },
            options,
          ),
        ),
        "code delivery repositories",
      );
    }

    async resolveCodeDeliveryRepositories(
      repositories: string[],
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryRepositoriesSnapshot> {
      return requireParsed(
        parseCodeDeliveryRepositories(
          await this.deliveryJson(
            "/code/delivery/repositories/resolve",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify({ repositories }),
            },
            options,
          ),
        ),
        "code delivery repositories",
      );
    }

    async queryCodeDeliveryPullRequests(
      query: CodeDeliveryPullRequestQuery,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryPullRequestsPage> {
      return requireParsed(
        parseCodeDeliveryPullRequestsPage(
          await this.deliveryJson(
            "/code/delivery/pull-requests/query",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(query),
            },
            options,
          ),
        ),
        "code delivery pull requests",
      );
    }

    async getCodeDeliveryPullRequestDetail(
      target: CodeDeliveryPullRequestTarget,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryPullRequestDetail> {
      return requireParsed(
        parseCodeDeliveryPullRequestDetail(
          await this.deliveryJson(
            "/code/delivery/pull-requests/detail",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(target),
            },
            options,
          ),
        ),
        "code delivery pull request detail",
      );
    }

    async runCodeDeliveryPullRequestAction(
      body: CodeDeliveryPullRequestActionBody,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryActionResult> {
      return requireParsed(
        parseCodeDeliveryActionResult(
          await this.deliveryJson(
            "/code/delivery/pull-requests/action",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
            options,
          ),
        ),
        "code delivery pull request action",
      );
    }

    async queryCodeDeliveryRuns(
      query: CodeDeliveryRunQuery,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryRunsPage> {
      return requireParsed(
        parseCodeDeliveryRunsPage(
          await this.deliveryJson(
            "/code/delivery/runs/query",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(query),
            },
            options,
          ),
        ),
        "code delivery runs",
      );
    }

    async getCodeDeliveryRunDetail(
      target: CodeDeliveryRunTarget,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryRunDetail> {
      return requireParsed(
        parseCodeDeliveryRunDetail(
          await this.deliveryJson(
            "/code/delivery/runs/detail",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(target),
            },
            options,
          ),
        ),
        "code delivery run detail",
      );
    }

    async runCodeDeliveryRunAction(
      body: CodeDeliveryRunActionBody,
      options?: DeliveryRequestOptions,
    ): Promise<CodeDeliveryActionResult> {
      return requireParsed(
        parseCodeDeliveryActionResult(
          await this.deliveryJson(
            "/code/delivery/runs/action",
            {
              method: "POST",
              headers: this.headers(true),
              body: JSON.stringify(body),
            },
            options,
          ),
        ),
        "code delivery run action",
      );
    }
  };
}
